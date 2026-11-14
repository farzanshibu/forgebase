# Choosing a backend

ForgeBase keeps its structured data in one of three setups. This page helps you
pick one, tells you where each one stops, when to add read replicas or shards,
and how to move between them. The mechanics (flags, files, settings) are in
[database.md](database.md) and [deployment.md §3.5](deployment.md#35-database-postgresql-or-sqlite);
the capability differences feature by feature are in [feature-parity.md](feature-parity.md).

Numbers below are from [benchmarks.md](benchmarks.md): **measured on an M4
laptop with SQLite; Linux and PostgreSQL numbers are TBD** until the `bench`
workflow has run (benchmarks §9). Treat them as orders of magnitude.

## 1. The short answer

| You are | Choose | Why |
|---|---|---|
| Trying it, building an app, running tests, a side project, an edge or single-tenant box | **SQLite** (`-lite` release, or `forgebase init --database sqlite`) | One file, in process, nothing else to run or size. Fast reads, small footprint. |
| Production for a team or product, one server | **Bundled PostgreSQL** (the default) | A real server engine under the same single binary: concurrent readers, extension accelerators (pgvector, Apache AGE, pgmq, pg_cron, pg_trgm), the SQL editor's PostgreSQL statements, `forgebase db psql`, and the way to replicas and shards later. No install. |
| You already run PostgreSQL (managed service, DBA team, backups and HA you trust) | **External PostgreSQL** (`forgebase init --database-url postgres://…`) | ForgeBase uses your server; you keep its operations. Hosted services often lack Apache AGE and pgmq: ForgeBase falls back to portable engines and the capability matrix says which. |
| Reads outgrow one server, or a dashboard and the SQL editor compete with the API | add **read replicas** (PostgreSQL only) | §4 |
| One writer is the bottleneck, or tenants must live on separate hardware | add **shards** (PostgreSQL only) | §5 |

If unsure, take the default (bundled PostgreSQL). It is the choice with a path
forward; moving from SQLite later is possible (§6) but is a planned, stopped-server
migration, so start on PostgreSQL if the project is meant to grow.

## 2. What each one is

| | SQLite | Bundled PostgreSQL | External PostgreSQL |
|---|---|---|---|
| Engine process | in the server | PostgreSQL 18, started and stopped by `forgebase serve`, Unix socket only | yours |
| Data | `<data_dir>/forgebase.db` (WAL) | `<data_dir>/postgres/data` | your server |
| Concurrency | one writer, a read pool (`database.read_pool_size`, default 8, 1–64) | one writer connection per database, `database.postgres.pool_size` read connections (default 16, 1–64) | the same |
| Durability setting | `database.synchronous = full` (fsync per commit; `normal` trades the latest commits for speed) | `synchronous_commit = on`, `wal_level = logical` | yours |
| Extensions | none: portable engines for vector (USearch), graph (SQL), queues, trigram | accelerators in `auto` mode, portable fallback if one is missing | what the server has |
| Replicas, shards | no | yes (managed local or remote nodes, external replicas, shard sets) | yes (replicas of `kind: external`; a shard is another PostgreSQL primary) |
| Operations you own | backup of one directory | the data directory; the engine is managed (extraction, roles, upgrade of the bundle) | the server, its upgrades, HA and backups |
| Footprint | the server alone | the server plus a PostgreSQL process tree (a first start unpacks about 30 MB) | the server alone |
| Version | n/a | the bundled engine (PostgreSQL 18); `database.postgres.engine_dir` can name an installed PostgreSQL 17 or newer instead | your server's (see [ADR-0028](adr/0028-bundled-postgres.md)) |

Which release carries what: the standard release embeds the engine and defaults
to PostgreSQL; the `-lite` release leaves it out and defaults to SQLite (a
`-lite` build answers `--database postgres` with a message that points at
`--database-url`). PostgreSQL cannot run as root: run ForgeBase as an
unprivileged user (the Docker image does).

## 3. Single-writer limits

ForgeBase is one server process and **the only writer** to its database
(ADR-0029 decision 19). On SQLite that is one write connection taking the write
lock (`BEGIN IMMEDIATE`); on PostgreSQL it is one writer connection per
database set. Reads run on a pool and do not wait for the writer.

What that means in practice:

- **Writes serialize.** Throughput is bounded by how fast one transaction can
  commit, and a commit waits for the disk. The Mac figures (benchmarks §4):
  64 µs per durable commit and about 2 200 creates per second over HTTP on 16
  connections. **Those commits only cost 65 µs because macOS `fsync` does not
  flush the drive's cache.** On Linux each commit waits for the device
  (benchmarks §9.2), so expect much lower single-row write rates there; batch
  where the API lets you (a transaction of 100 rows commits at 181 000 rows/s on
  the Mac, against 15 500 single commits/s).
- **A slow disk is the cap, not the CPU.** On the external USB disk with
  stalling flushes the same create went from 2 ms to 70 ms at the median
  (benchmarks §4.2). Put the data directory on a local SSD.
- **Reads scale with the pool and with cores**: 46 000 primary-key reads per
  second in process, 6 500 list pages per second over HTTP on one connection
  (benchmarks §4). On SQLite they never block on the writer (WAL).
- **Long writes block other writes.** A multi-second write transaction (a big
  import, a heavy SQL-editor statement) holds up every other write. Run bulk
  work in batches or through jobs.
- **Other services share the writer.** Realtime events are rows in the writing
  transaction and queue publishes go through the writer; the KV engine answers
  from memory and writes behind, so its reads and writes are not limited by it.

You will meet this limit when write latency (p95 of `forgebase_db_query_duration_seconds{mode="write"}`)
climbs with load while CPU stays low. More connections do not help; faster
storage, batching, or sharding (§5) do.

## 4. When to add read replicas

Replicas (ADR-0029 §2) are PostgreSQL streaming replicas the server routes
**reads** to. They scale reads and isolate heavy readers; they do not scale
writes.

Add replicas when:

- the primary's CPU is high from reads, or readers queue for a pool connection
  (`forgebase_db_pool_connections_in_use{pool="reader"}` near the pool size, the
  read p95 `forgebase_db_query_duration_seconds{mode="read"}` rising);
- dashboards, the SQL editor ("Read from replica"), graph and storage metadata
  reads, queue browsing or analytics should not compete with the API;
- you want a warm copy for a manual failover (below).

Do not add them to fix write latency, to survive a disk failure (a replica is
not a backup: [database.md §5](database.md#5-backups-and-rollback)), or when
all your traffic is writes.

What to know:

- **Reads never go stale for the caller who wrote.** A write returns
  `X-ForgeBase-Commit`; later reads carry it (SDKs do) or the server remembers
  the caller's last commit for 10 s, and a replica serves the read only after it
  has replayed that position (it waits up to `lsn_wait_ms`, 50 ms, then the read
  goes to the primary). `X-ForgeBase-Consistency: strong` forces the primary.
  Other callers may see data a few milliseconds old.
- **Lag limits.** A replica above `max_lag_ms` (2 000) or `max_lag_bytes`
  (64 MiB) leaves the rotation and returns after three samples under half of
  that. With every replica out, reads use the primary: nothing fails because a
  replica is gone. A replica read that is cancelled by a recovery conflict is
  retried once on the primary.
- **Failover is manual** (ADR-0029 §2.8): promote a replica from the API or the
  dashboard, then restart the server so its connections follow. There is no
  automatic failover in this release.
- **Where they run.** `kind: local` clones a replica on the same host (for
  trying it and for read isolation; it shares the host's CPU, memory and disk),
  `kind: remote` runs it on another host through `forgebase node join` (needs
  TLS between hosts), `kind: external` points at a server you run. The
  Kubernetes chart pins one replica of the pod
  ([deployment.md §9.1](deployment.md#91-why-one-replica)).
- **Cost.** `wal_level = logical` adds about 1.4 % WAL (ADR-0029 addendum). A
  replica needs the primary's data size in disk plus WAL, and a slot keeps up
  to `slot_wal_keep_size` (8 GB) of WAL for one that is behind.
- **The numbers** (lag under load, how many reads land on replicas, fallbacks)
  come from the topology job in the `bench` workflow (benchmarks §9.3), Linux
  TBD. Until then, watch `GET /api/system/database/topology` (`lag_ms`,
  `lag_bytes`, `in_rotation`) on your own load.

## 5. When to add shards

Sharding (ADR-0029 §3) keeps the **control** database (identity, configuration,
topology) on the primary set and places **tenants** (an environment of a
project) on shard sets, each a separate PostgreSQL primary with its own single
writer and its own replicas. It scales writes and size by tenant.

Add shards when:

- one writer's commit rate or one database's size is the limit and the data
  belongs to many tenants (tenant-scoped collections, per-tenant storage,
  graph, queues);
- tenants need to live on separate hardware or in separate failure domains;
- a few large tenants should move off a shared server (online tenant moves:
  logical replication, a write freeze of at most 10 s, verification, then a
  placement flip; plans estimate `150 ms + 4 ms per 1 000 rows`).

What to know:

- **It does not help a single big tenant** (one tenant is one set). A query
  runs on one set: the SQL editor picks the set for a read, and statements that
  write run on control only. Data that is not tenant-scoped stays on control.
- **Placement** is least-loaded or "control" (`[database.sharding] placement`),
  with project affinity so a project's three environments share a set.
- **Complexity.** Every schema change runs on every set; two-phase transactions
  need `max_prepared_transactions`; backups and moves have more parts. A
  single PostgreSQL primary with replicas goes a long way first.
- Shards are PostgreSQL only, and `forgebase db convert --to sqlite` refuses a
  cluster with more than one set until the shards are drained and removed.

## 6. Moving between backends: `forgebase db convert`

`forgebase db convert --to postgres|sqlite` moves a **stopped** project to the
other backend (ADR-0028 §6.4): a snapshot of the source first (`--no-backup`
skips it), a table-level copy with a verification of row counts and a SHA-256
per table, the marker and `forgebase.toml` updated, and the old database kept
(`forgebase.db.pre-postgres`; never deleted by the tool). Run it from the
project directory with the server stopped.

```sh
forgebase backup                      # your own snapshot as well, before anything
forgebase db convert --to postgres    # SQLite -> bundled PostgreSQL
forgebase serve                       # starts the engine and the server
```

**SQLite to PostgreSQL** is the common direction and the supported growth path.
Vectors, queue deliveries and schedules arrive as portable rows; with the
extensions in `auto` mode the next start moves them to pgvector, pgmq and pg_cron
where the engine has them. Problems in the source (integrity or foreign-key
violations, a collection name PostgreSQL rejects) are refused before anything
is copied.

**PostgreSQL to SQLite** first converts the data the source keeps in
extensions (pgvector vectors, pgmq deliveries, pg_cron schedules) to portable
rows in the source itself (`--no-prepare` skips that and refuses when such data
exists). A failed attempt after that step leaves a valid PostgreSQL project on
portable rows.

**Known refusals.** The tool lists every problem in one run so you can fix them
all. Things that stop a conversion:

| Direction | Refused | What to do |
|---|---|---|
| to PostgreSQL | **a trigram index**: the SQLite engine keeps a side table `_forgebase_trgm_<index>`, and the table pairing stops with `table _forgebase_trgm_<index> exists on SQLite but not on PostgreSQL` | drop the trigram index (`DELETE /api/v1/indexes/<name>` or the dashboard) before converting, and create it again on PostgreSQL afterwards; `fuzzy` answers `no_trigram_index` in between. Known gap, [feature-parity.md](feature-parity.md) |
| to PostgreSQL | a table or column that exists only on SQLite (hand-made through the SQL editor), or an automatic index name the target derives differently | recreate it by hand on the target after converting, or drop it |
| to PostgreSQL | integrity or foreign-key violations in the SQLite file; a collection name PostgreSQL rejects | fix the data (`PRAGMA integrity_check`, `foreign_key_check`), rename the collection |
| to SQLite | **a trigram index**: `trigram index <name> on <collection> needs pg_trgm, which SQLite does not have: drop the index first (fuzzy search is PostgreSQL only)` | drop it first; the portable SQLite engine can create one again afterwards (feature-parity) |
| to SQLite | more than one database set (shards), or any replica | drain and remove the shards, remove the replicas |
| to SQLite | objects that exist only on PostgreSQL: hand-written row-level security policies, functions, triggers, types and domains, free-standing sequences, views that are not view collections, columns of types SQLite lacks (with data in them) | remove or recreate them; the message names each |
| to SQLite | data still held by an extension: pgvector vectors, pgmq deliveries, pg_cron schedules (only with `--no-prepare`) | run without `--no-prepare`, or the `vector.convert` and `queue.convert` jobs with `{"to": "portable"}` |

A derived Apache AGE graph is dropped, not refused: the canonical graph tables
are copied and AGE is rebuilt from them on use. Replicas and shards are not
converted; a PostgreSQL project with them cannot go to SQLite without removing
them first.

**Plan a conversion** like a migration: take your own backup and test the
restore ([database.md §5](database.md#5-backups-and-rollback)), convert a copy of
the data directory first if the project is large (the copy is table by table
and the time grows with the data), keep the `.pre-postgres` file until you have
run on the new backend for a while, and re-run your acceptance checks (the 25
scenarios in `tests/integration` run on both backends in CI).

**Going from bundled to external PostgreSQL** is not a `convert`: both are
PostgreSQL. Use a PostgreSQL dump and restore into your server, then point a new
project at it with `--database-url` (see [deployment.md §3.5](deployment.md#35-database-postgresql-or-sqlite));
`forgebase db psql` and `pg_dump` against the bundled engine's socket give you
the data.

## 7. A decision in five questions

1. **Is it for production, or will it grow?** Yes: PostgreSQL. No: SQLite is fine.
2. **Do you already operate PostgreSQL well?** Yes: external. No: bundled.
3. **Do you need vector search at scale, openCypher beyond the portable subset,
   or pgmq-backed queues?** PostgreSQL with extensions (bundled: all five
   accelerators; external: whichever the server has).
4. **Do reads or writes limit you?** Reads: replicas (§4). Writes or size across
   many tenants: shards (§5). Writes on one tenant: faster disk, batching, a
   bigger machine.
5. **Will someone need to leave PostgreSQL later?** SQLite as a target works only
   for a single set without replicas and without trigram indexes (§6).

[capacity-planning.md](capacity-planning.md) turns the choice into CPU, memory,
disk and connection numbers.
