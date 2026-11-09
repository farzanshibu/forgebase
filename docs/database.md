# ForgeBase database

ForgeBase keeps its structured data in one embedded SQLite file, in WAL mode.
Other data types sit beside it in the same data directory. This page covers
the storage engine, the data-directory files, the multi-model layout, the
system migrations, and the backup pointers.

Decisions: [ADR-0001 §2–5](adr/0001-architecture.md) (engine, concurrency,
migrations), [ADR-0003](adr/0003-data-core.md) (collections and records),
[ADR-0005](adr/0005-kv.md) (key-value), [ADR-0006](adr/0006-graph.md) (graph),
[ADR-0012](adr/0012-vector.md) (vector), [ADR-0018](adr/0018-object-storage.md)
(object storage).

## 1. Engine and concurrency

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
├── forgebase.db (+ -wal, -shm)  canonical: structured data
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
- **Manual copy.** Stop the server, then copy `forgebase.db`, `forgebase.db-wal`
  (if present), `storage/`, and `auth/master.key`. Copying the database file
  while the server runs can produce an inconsistent copy.
- **Full backup and restore commands** are work package FB-83 and are not
  built yet.

`forgebase doctor` checks database integrity, free disk space, file permissions,
migration state and the port. It exits with code 1 when a check fails.

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
