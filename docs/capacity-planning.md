# Capacity planning

How much CPU, memory, disk and how many connections a ForgeBase server needs for
a workload, from the numbers we have, and a worksheet to estimate yours.

**Read this first.** Every measured figure here comes from
[benchmarks.md §4](benchmarks.md) and is labelled **"measured on M4 laptop; Linux
TBD"**: one run on a shared Apple M4 (10 cores, 16 GiB), SQLite, with the client
on the same machine. There are no Linux numbers and no PostgreSQL load numbers
yet; the `bench` workflow ([benchmarks §9](benchmarks.md#9-linux-scale-validation-the-bench-workflow))
produces them and its `resources` output (CPU, memory, file descriptors, disk
per scenario) is what turns the estimates below into measurements. Anything
marked **estimate** is arithmetic or judgment from the configuration, not a
measurement. Use the numbers to choose a starting size, then measure your own
load and adjust (§6).

## 1. What was measured, and what it does not tell you

| Figure | Value | Label |
|---|---|---|
| Records CRUD over HTTP, 16 connections | 2 190 creates, reads, updates and deletes per second each (8 700 requests/s); create p99 4.8 ms, read p99 0.3 ms | measured on M4 laptop; Linux TBD |
| Single durable commit | 64 µs (15 500 per second), 100 rows in one commit: 181 000 rows/s | measured on M4 laptop; **not valid for Linux**: macOS `fsync` does not flush the cache (benchmarks §9.2) |
| Primary-key read in process | 21.6 µs (46 000 per second) | measured on M4 laptop; Linux TBD |
| Indexed list page of 20, one HTTP connection | 6 500 per second, p99 0.3 ms | measured on M4 laptop; Linux TBD |
| Queue publish 1 / batch of 10 / pull+ack 100 | 2 300 / 7 600 / 9 500 messages per second | measured on M4 laptop; Linux TBD |
| Realtime fan-out | 3 600 events per second in, 108 000 deliveries per second out to 30 subscribers, p99 3.4 ms | measured on M4 laptop; Linux TBD |
| Key-value | 859 000 GET and 591 000 SET per second in process | measured on M4 laptop; Linux TBD |
| Vector top-10 (HNSW, 10 000 × 128) | 152 µs; 6 600 per second | measured on M4 laptop; Linux TBD |
| Token verify / password sign-in | 27 µs / 15 ms (Argon2id); HTTP sign-in is capped by the per-IP limiter | measured on M4 laptop; Linux TBD |
| Resident memory of the release server (SQLite) | 82 MB after the full load run | measured on M4 laptop; Linux TBD |
| Recovery after `kill -9` | 0.55 s to ready, 0 acknowledged writes lost | measured on M4 laptop (debug binary); Linux TBD |
| `wal_level = logical` overhead | +1.4 % WAL | measured (ADR-0029 addendum, bundled PostgreSQL 18) |
| Replica lag, read share, shard routing, storage, graph, SQL, fuzzy search, CPU, fds, disk | not measured | **TBD**: `bench` workflow |

Limits of these numbers: client and server shared the CPU (so the server could
do more), the machine was busy with other work (so every figure is soft), the
test data was small and warm in cache, and CPU, disk and file descriptors were
not sampled. They show where ForgeBase is not the bottleneck; they do not show
where it saturates.

## 2. CPU

**What uses it.** Request handling and JSON, SQL execution (SQLite in process,
or PostgreSQL's processes beside the server), password hashing (Argon2id,
deliberately 15 ms of one core per sign-in), TLS if the server terminates it,
WebSocket fan-out, WebAssembly and JavaScript functions, index maintenance
(vector HNSW build, trigram tables), and background jobs (reconcilers,
schedulers).

**Sizing rule.** `cores = peak requests per second / requests per second per
core`, plus headroom. We do not yet have a requests-per-core figure: CPU was not
sampled. The `bench` workflow's `resources` rows give it directly (for each
scenario, `rps / (cpu_pct_p50 / 100)`, for SQLite the process and for
PostgreSQL the whole tree).

**Starting points (estimate).** From the M4 throughput, and a presumption that
a modern server core is within a small factor of one M4 performance core:

| Workload | Starting size | Reasoning |
|---|---|---|
| Development, a small app, up to about 100 requests/s | 1 vCPU, 1 GiB | the 82 MB footprint and thousands of requests per second on a laptop leave a very wide margin |
| A product API, up to about 1 000 requests/s mixed, some realtime | 2 vCPU | the laptop did 8 700 req/s with the client using cores as well; this keeps a ten-fold margin for Linux flush costs, TLS and PostgreSQL's own processes |
| 1 000 to 5 000 requests/s, heavy realtime fan-out, functions or password sign-ins | 4 vCPU | fan-out multiplies deliveries (108 000 per second out of 3 600 in); sign-ins cost one core per ~65 per second |
| More than that, or write-bound | measure, then §5 | the single writer, not the CPU, is usually the limit (choosing-a-backend §3) |

Sign-ins: `cores for logins = sign-ins per second × 0.015`. A login spike of 200
per second needs 3 cores for Argon2id alone; the per-IP rate limiter (ADR-0004)
bounds one client address.

On PostgreSQL add the engine's processes (a background-writer set plus one
process per connection) to the tree: size for the server and the engine
together, and give PostgreSQL room for parallel readers (`database.postgres.pool_size`,
default 16).

## 3. Memory

| Part | Size | Label |
|---|---|---|
| ForgeBase server, SQLite, after a load run | 82 MB resident | measured on M4 laptop; Linux TBD |
| Bundled PostgreSQL: `shared_buffers` | 128 MB (set by ForgeBase; `database.postgres.settings` overrides it) | configuration |
| Bundled PostgreSQL: connections | `max_connections = 64`; each backend takes memory as it works, a few MB when idle to `work_mem` multiples while sorting | configuration; **estimate** for the rest |
| PostgreSQL page cache | the OS cache: more RAM here is the cheapest way to speed reads | general PostgreSQL behaviour |
| Key-value engine | all keys and values live in memory (`kv` memory limit and LRU, ADR-0005) | configuration |
| Vector index (HNSW) | derived files in `indexes/`, memory-mapped; about `vectors × (dims × 4 B + links)`; 10 000 × 128 is about 5 MB of vector data plus the graph | **estimate** |
| Realtime | a small buffer per connection and subscription (backpressure limits bound it) | **estimate** |

A light smoke run of a **debug** build showed the process tree at 180 to 195 MB
on SQLite and 400 to 470 MB with the bundled PostgreSQL (and about 700 MB with
two replicas and a shard on one host). Debug builds are bigger than the 82 MB
release figure; treat those as upper bounds for a lightly loaded server, not as
benchmarks.

**Starting points (estimate).** SQLite: 512 MB plus the size of the hot part of
the database and of the KV data. Bundled PostgreSQL: 2 GiB for the server and the
engine, plus room for the hot data in the OS cache, plus the KV working set.
Every replica or shard on the same host adds an engine's worth (roughly 300 MB
idle) plus its cache. If memory is short, lower `database.postgres.pool_size`
and keep `max_connections` as set before raising `shared_buffers`.

## 4. Disk

Disk matters twice: **space** (§4.1) and **flush latency** (§4.2).

### 4.1 Space

Plan for: data, indexes, WAL, object blobs, backups, and growth. The worksheet
(§6) multiplies it out. Rules of thumb (**estimates**, check with your data):

| Item | Rule of thumb |
|---|---|
| Records (collections) | row bytes × rows, plus about 30 to 100 % for B-tree indexes on the filtered and sorted fields; JSON documents carry their field names in every row |
| Trigram (fuzzy) indexes | the portable engines keep a table with a row for each distinct trigram of each indexed value: about the value's length in rows, a few dozen bytes each, so an indexed text field costs several times its own size. On PostgreSQL with `pg_trgm` a GIN index is smaller |
| Vectors | `dims × 4 B` per vector in the database as the canonical copy, plus an HNSW index of about the same again |
| Object storage | the bytes of every distinct object, content-addressed (identical bodies are stored once), plus versions if the bucket keeps them; metadata is a row per version |
| Queues and jobs | one row per message and per delivery until acknowledged and retained; backlog and retention drive it |
| Realtime outbox, audit log, usage events | append-heavy: set retention |
| PostgreSQL WAL | `max_wal_size` plus, per replica, up to `slot_wal_keep_size` (8 GB) for a lagging one; `wal_level = logical` adds about 1.4 % |
| SQLite WAL | small; checkpointed at shutdown |
| A replica | a full copy of its set |
| Backups | each snapshot is roughly the database plus `storage/`; keep as many as your retention needs ([database.md §5](database.md#5-backups-and-rollback)) |

Leave 25 to 30 % free: PostgreSQL needs space for vacuum and rewrites, `forgebase
migrate` takes a pre-migration snapshot, and index rebuilds make a second copy
while they run. `fb-load` reports `disk_mb` start, end and max of the data
directory per scenario so you can measure growth per operation in your own
setup.

### 4.2 Flush latency (the part that surprised us)

A commit waits for the disk, so **write throughput per writer is about
`1 / commit latency`** for single-row commits. On the Mac that is 65 µs (cheap
flush); on Linux it is the device's flush time: sub-millisecond on a local NVMe
SSD, several milliseconds on many network or virtual disks, and tens of
milliseconds to seconds on a disk that stalls (the USB disk in benchmarks §1
went from 2 ms to 70 ms per create). Use a local SSD, keep the data directory
off shared or throttled volumes, and run the fsync probe the `bench` workflow
prints on your target disk: `writes per second per connection ≈ 1000 / fsync ms`
bounds the single-row write rate, and batching raises it (benchmarks §9.2).
SQLite `synchronous = normal` and PostgreSQL's `synchronous_commit` trade the
latest commits on power loss for speed; the defaults are the safe ones.

## 5. Connections and descriptors

| Where | Limit | Setting |
|---|---|---|
| HTTP clients to ForgeBase | not capped by ForgeBase; bounded by file descriptors and memory (about one descriptor per connection) and by your proxy | `ulimit -n` / `LimitNOFILE` |
| ForgeBase to the database (SQLite) | 1 writer, `read_pool_size` readers (default 8) | `database.read_pool_size` |
| ForgeBase to PostgreSQL | 1 writer, `pool_size` readers (default 16, 1–64) per database set; a replica gets its own pool | `database.postgres.pool_size` |
| PostgreSQL itself (bundled) | `max_connections = 64` | `database.postgres.settings` |
| Realtime (WebSocket or SSE) per user or key | 32 | `realtime.max_connections_per_principal` |
| Redis protocol (RESP) | 1 024 | `resp.max_connections` |
| Sign-in attempts per client IP | rate limited | ADR-0004 |

The database pool is the real concurrency limit: requests beyond it queue for a
connection (watch `forgebase_db_pool_connections_in_use` and `_idle`, and
`forgebase_db_query_duration_seconds`). More HTTP connections than pool slots
just wait. Bounds to check: PostgreSQL's `max_connections` (64) must cover
`1 + pool_size` per set the server uses, plus the engine's own processes and
`forgebase db psql` sessions; with shards each set has its own PostgreSQL, so
the limit is per cluster.

File descriptors: `fds ≈ HTTP connections + realtime sockets + RESP sockets +
database sockets + open files (SQLite files, blobs being streamed, logs) +
a few dozen`. The default `ulimit -n` of 1 024 is a hazard for more than a few
hundred connections; raise it (the systemd unit and Docker image do not set it
for you). `fb-load` samples the open descriptors of the server process tree, so
you can read the fds at a given load from the `resources` rows (on Linux).

## 6. Worksheet

Fill the blanks for your workload; the formulas assume a steady peak. Do it
twice: today, and at your planning horizon (12 months).

**A. Traffic**

| | Today | Horizon |
|---|---|---|
| Peak API requests per second, reads (R) | | |
| Peak API requests per second, writes (W) | | |
| Sign-ins per second at peak (L) | | |
| Realtime subscribers (S), events per second published (E) | | |
| Objects uploaded per day, average size | | |

**B. Does one writer suffice?** Single-row write rate cap ≈ `1000 / fsync ms`
per writer connection (§4.2); batching multiplies it. If `W` is within half of
the cap you have a write-bound system: prefer PostgreSQL on a fast local SSD,
batch writes, and plan shards (choosing-a-backend §5) before the cap, not at it.

**C. CPU.** `cores = (R + W) / requests-per-core + L × 0.015 + E × S × 0.0001`
(the last term is an **estimate**: fan-out at about 10 000 deliveries per core
per second, conservative against the 108 000 per second a loaded laptop
managed with several cores busy). Add the database engine's cores on PostgreSQL.
Fill requests-per-core from the first Linux `resources` run; until then use the
starting points in §2. Add 30 % headroom.

**D. Memory.** `server + engine + hot data + KV`: §3 starting point, plus the
resident KV data (`keys × (key + value + ~100 B)` **estimate**), plus the
working set you want cached.

**E. Storage growth.**

```text
per_month_bytes  = (records_per_month   × avg_row_bytes   × (1 + index_factor))   # index_factor 0.3 to 1.0
                 + (text_rows_per_month × avg_text_bytes  × trigram_factor)        # trigram_factor 3 to 6 (portable)
                 + (vectors_per_month   × dims × 4        × 2)                     # canonical + HNSW
                 + (objects_per_month   × avg_object_bytes × dedup_factor)         # dedup_factor 1.0 = no sharing
                 + queue/audit/outbox retained rows × row bytes
disk_needed      = (current_bytes + months × per_month_bytes) × (1 + replicas that live on this disk)
                 + WAL (max_wal_size + slot_wal_keep_size per replica)
                 + backups kept × snapshot_size
                 → divide by 0.7 to leave room
```

Take `avg_row_bytes` and the index factors from your own database
(`forgebase db status` gives its size; the dashboard's index statistics and the
`dbstat` data on SQLite show per-index sizes) after loading a representative
sample, instead of the rules of thumb.

**F. Connections.**

```text
database_connections = 1 writer + pool_size readers            (per database set; replicas add their own pool)
postgres max_connections ≥ sets_on_this_cluster × (1 + pool_size) + 10
descriptors          ≥ clients + S + RESP clients + database_connections + 200
```

If `1 + pool_size` approaches `max_connections` (64 by default), reduce
`pool_size` or raise `max_connections` and memory together.

**G. Verify.** Run the `bench` workflow or `fb-load` against a staging copy with
your data sizes and read `resources`: CPU p50 and max, RSS, open fds, disk growth
per scenario. If CPU p50 at your peak rate is above 60 % or RSS is within 25 % of
the machine's memory, size up. Re-run after major changes (an upgrade, a new
index, a new data model).

## 7. Examples (estimates, not measurements)

| Workload | Shape | Starting point |
|---|---|---|
| Prototype or internal tool | 5 users, 10 requests/s, 100 MB of data | SQLite, 1 vCPU, 512 MB, 10 GB disk |
| Small SaaS | 1 000 daily users, 50 requests/s peak, 5 GB of data, light realtime | bundled PostgreSQL, 2 vCPU, 4 GiB, 50 GB local SSD, nightly backups to other storage |
| Busy product | 200 requests/s peak, 100 GB, heavy dashboards and analytics reading | bundled PostgreSQL, 4 vCPU, 16 GiB, 500 GB local NVMe, plus one read replica for the dashboards |
| Large multi-tenant | thousands of tenants, write-bound | PostgreSQL with shards (choosing-a-backend §5), a replica per set that needs it; measure first |

All of these should be validated with the §6.G step before you commit to hardware.
