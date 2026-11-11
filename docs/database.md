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
| Access | Unix socket only; TCP is off unless `[database.postgres] listen` is set | in-process |
| Extras | Extensions as accelerators with a portable fallback (ADR-0029), `forgebase db psql` | none |

`forgebase init` asks `Database: [1] PostgreSQL (default, bundled server) [2]
SQLite (one file)` on a terminal and takes the build's default otherwise. The
data directory's backend marker (`forgebase.backend`) wins over the default at
every start: an existing SQLite project stays SQLite, and `forgebase status`
and `forgebase doctor` say so with the command that moves it,
`forgebase db convert --to postgres` (ADR-0028 §6.4). PostgreSQL cannot run as
root. [Deployment §3.5](deployment.md#35-database-postgresql-or-sqlite) has the
details and [Upgrading to 0.3](release-notes/0.3.md#upgrading-to-03) the steps.

The rest of this page describes SQLite unless a section says otherwise; the
PostgreSQL engine's settings and commands are in the deployment guide.

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
`.forgebase.lock`. `migrate` and `init` take the same lock; `status` and
`doctor` do not.

## 2. Files in the data directory

```text
<data_dir>/
├── forgebase.backend             which engine this directory holds
├── forgebase.db (+ -wal, -shm)  SQLite only, canonical: structured data
├── postgres/                     PostgreSQL only: engine, data/, run/ (socket), roles.secret
├── storage/                      canonical: object blobs (when storage is in use)
├── auth/master.key               canonical: root key for auth secrets (see §5)
├── indexes/                      derived: rebuildable indexes
├── backups/                      pre-migration snapshots
├── tmp/                          staging for in-flight writes; safe to delete when stopped
└── .forgebase.lock               held by the running server
```

`auth/master.key` is created by the auth service on first start. It is not
in the `DataLayout` directory list, so `init` does not create it. Losing it
makes encrypted signing keys and TOTP seeds unreadable. Copy it with the
database.

## 3. Multi-model data

One database file holds several data models. Each one is a set of tables with
the `_forgebase_` prefix, except user collections.

| Model | Where it lives | Notes |
|---|---|---|
| Records (collections) | One SQL table per user collection; definitions in `_forgebase_collections` and `_forgebase_schema_history`; `_forgebase_indexes`; history in `_forgebase_record_versions` | Typed fields; filter language compiled to parameterised SQL. ADR-0003. |
| Key-value | `_forgebase_kv_entries`, one row per key within a namespace | Strings, hashes, lists, sets, sorted sets, streams, TTL, LRU eviction under a memory budget. ADR-0005. |
| Graph | `_forgebase_graph_nodes`, `_forgebase_graph_edges` (indexed both directions) | Tenant-scoped. ADR-0006. |
| Vector | `_forgebase_vector_collections`, `_forgebase_vector_fields`, `_forgebase_vector_items`, `_forgebase_vector_values` | Canonical vectors are in SQLite. ANN indexes are derived and rebuildable. ADR-0012. |
| Object storage | Metadata in `_forgebase_storage_*`; bytes in `storage/` | Blobs are not stored in SQLite. ADR-0018. |
| Jobs and workflows | `_forgebase_jobs`, `_forgebase_job_events`, `_forgebase_job_schedules`, `_forgebase_job_workers`, `_forgebase_job_queues`, `_forgebase_job_workflows`, `_forgebase_job_deps`, `_forgebase_job_attempts` | A job is a row; every state change is a committed transaction. ADR-0007, ADR-0023. |
| Realtime | `_forgebase_realtime_outbox` | One row per record change, written in the same transaction as the change. ADR-0011. |

## 4. System migrations

Migrations are SQL files in `migrations/system/`. They are embedded in the
binary and applied at startup or by `forgebase migrate`. Numbers are
strictly increasing; the gaps at 0010 and 0021 have no file in this tree.

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
| `0022_jobs_long_tasks.sql` | Job queues, workflows, dependencies, attempts |

The `_forgebase_migrations` table records each applied file with its name,
SHA-256 checksum, time and duration. `PRAGMA user_version` mirrors the latest
version for external tools. Check the current state with:

```sh
forgebase migrate --status
```

### Rules for migrations

These rules are in [`migrations/README.md`](../migrations/README.md).

1. File names are `NNNN_snake_case_name.sql`. Register every file in
   `SYSTEM_MIGRATIONS` in `crates/forgebase-migrations/src/lib.rs`; a test fails
   otherwise.
2. **Never edit a migration that has shipped.** A changed checksum makes the
   server refuse to start. Fix forward with a new migration.
3. Each migration runs in one `BEGIN IMMEDIATE` transaction with its bookkeeping
   row, followed by `PRAGMA foreign_key_check`. Statements that cannot run in a
   transaction (`VACUUM`, `PRAGMA journal_mode`) are not allowed.
4. Migrations are forward-only. A database migrated by a newer binary is refused.

## 5. Backups and rollback

- **Pre-migration snapshot.** Before pending migrations run on an existing
  database, the server writes `backups/pre-migrate-v<N>-<time>.db` with
  `VACUUM INTO`. Restoring that file is the rollback for a bad migration.
- **Backup (online).** `forgebase backup` copies the whole data directory while
  the server keeps serving: the database with `VACUUM INTO` (one consistent
  read snapshot; readers are never blocked), then `storage/` blobs and the
  derived `indexes/`. The copy goes to `<dest>.partial` and is renamed after
  `MANIFEST.json` (every file with size and SHA-256) is on disk, so a crash
  never leaves something that looks complete. Without a destination it writes
  a **snapshot** to `backups/snapshots/<UTC time>`; `--keep-last N` and
  `--max-age-days D` apply a retention policy afterwards (`backup prune` and
  `backup delete <id>` do it on demand; `backup list` shows what exists).
  Snapshots are also available over REST: `/api/v1/system/backups`
  (administrators only; see [api.md](api.md)).
- **Verify.** `forgebase backup verify <dir|id|latest>` re-hashes every file,
  rejects missing and unlisted files, opens the copied database read-only and
  runs `integrity_check`, `foreign_key_check` and the migration check.
- **Restore.** `forgebase restore <dir|snapshot id|latest|time> [--force]`,
  with the server stopped (it takes the data-directory lock). The backup is
  verified first, staged and hashed again, then swapped in. The data it
  replaces is **moved**, never deleted, to `backups/pre-restore-<time>/`. A
  crash mid-swap leaves a journal; the next `restore` (or
  `restore --recover`) rolls the previous data back, and `forgebase doctor`
  reports it. Giving a time restores the newest snapshot taken at or before it
  (point-in-time restore).
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
- **Manual copy.** Stopping the server and copying `forgebase.db`,
  `forgebase.db-wal` (if present), `storage/` and `auth/master.key` still works.
  Copying the database file while the server runs can produce an inconsistent
  copy; use `forgebase backup` instead.

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
it. Import needs the server stopped; export does not. Records of auth
collections are not exported (password hashes stay in the database), and
`created`/`updated` are reassigned on import, as in the REST import.

### `forgebase doctor`

Prints one line per check (`[ok]`, `[warn]`, `[FAIL]`) or JSON with `--json`
(`{"ok", "failed", "warnings", "checks": [{"name", "status", "detail"}]}`) and
exits with code 1 when a check fails. Checks: `config`, `data_dir`
(permissions), `database` (`integrity_check`, `foreign_key_check`, WAL mode,
file mode), `migrations`, `storage`, `indexes`, `disk` (free space),
`master_key` (presence, owner-only), `restore` (an interrupted restore),
`snapshots` (count and age of the newest; `--deep` also verifies it),
`staging` (leftovers of unfinished backups or restores), `index_files`
(vector index files readable), `wal` (size), `storage_blobs` (referenced blobs
that are missing: fail; files without metadata: warn) and `port`.

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

Pagination, `fields` and `expand` are in [api.md §7](api.md#7-lists-and-pagination).
