# ForgeBase benchmarks (FB-81)

Measured 2026-11-10 on the build of branch `agent/worker-fb81` (main at
`2b4d8f1` plus FB-81). Rule of the work package: **measure, do not optimise.**
Nothing in the server was changed. Misses are listed in §6 with their profile.

## 1. Hardware and environment

| Item | Value |
|---|---|
| Machine | Apple Mac16,10 (M4) |
| CPU | Apple M4, 10 cores (4 performance + 6 efficiency) |
| RAM | 16 GiB |
| OS | macOS 27.0.1 (Darwin 27.0.0, arm64) |
| Toolchain | rustc 1.99.0, cargo 1.99.0; release profile (`lto = "thin"`, `codegen-units = 1`) |
| Internal disk | APFS SSD (`/System/Volumes/Data`). `F_FULLFSYNC` p50 3.9 ms, p95 4.2 ms |
| External disk | USB-attached APFS SSD (the project disk). `F_FULLFSYNC` p50 87 ms, p95 3.2 s, max 3.8 s |
| SQLite settings | server defaults: WAL, `synchronous = FULL` (ADR-0001) |
| Loopback | client and server on the same machine, `127.0.0.1` |

Caveats that matter when reading the numbers:

- **The machine is shared.** Other agents compile and run tests on it at the same
  time (load average 2.8 to 5.3 during the runs). Treat every figure as
  "on a busy laptop". Re-run on a quiet machine before quoting externally.
- **Storage dominates anything that commits.** The external disk stalls for
  seconds at times, so all write-bound results are given for the **internal
  disk** (the reference) and, where it changed the picture, for the external disk.
  CPU-bound results (auth, policy, KV, vector, reads) do not depend on the disk.
- **macOS `fsync` is cheap.** SQLite calls plain `fsync`, which on macOS does not
  flush the drive's write cache (only `F_FULLFSYNC` does). So a "durable" commit
  here costs about 65 µs on the internal disk. Linux will be slower, by roughly
  the device's flush latency. Do not publish the write numbers as ForgeBase
  figures for Linux servers; re-run there.
- Client and server share the same CPU, so HTTP numbers are a lower bound for
  the server.

## 2. Method

Three tools (§9 adds benches, scenarios and a CI job to them), all in the
dev-only crate `crates/forgebase-bench` (nothing in the server depends on it;
the only new dependency is `criterion`, dev-only, MIT/Apache-2.0).

1. **Criterion benches** (`cargo bench -p forgebase-bench`), in-process, real
   SQLite in a temp directory, production pragmas and migrations. One file per
   area: `db`, `records`, `auth_policy`, `kv`, `vector`, `queue_jobs` (and
   `storage`, `graph`, `sql`, `fuzzy` since §9; queue and jobs run through
   `forgebase_server::start`, the real lifecycle).
   Values below are the Criterion median (middle of the three numbers). Run with
   `TMPDIR=<dir on the disk under test>`.
2. **HTTP load harness** `fb-load` (`crates/forgebase-bench/src/bin/fb-load/`,
   raw HTTP/1.1 keep-alive, no client library) against the release binary.
   `crates/forgebase-bench/run-load.sh <forgebase> <fb-load> <empty dir> [concurrency] [seconds]`
   creates a project and an administrator, starts the server, and runs
   `records`, `queue`, `realtime` and `auth`. Used here: 16 concurrent
   connections, 10 s per scenario, 30 realtime subscribers.
   - `records`: per worker, create then read then update (PATCH) then delete.
   - `queue`: publish of 1 and of 10 messages, then pull and ack in batches of 100.
   - `realtime`: 30 WebSocket subscribers on one collection, 4 writers; latency
     is the time from the client building the record to the subscriber receiving
     the event (same clock).
   - `auth`: `POST /api/auth/login` in a loop. The server limits sign-in attempts
     per client IP (by design), so most replies are `429`; the harness counts
     them separately.
3. **Recovery tests** `tests/integration/recovery.rs` (attached to
   `forgebase-cli`; `cargo test -p forgebase-cli --test recovery -- --nocapture`).
   Starts the real binary, runs 8 writer threads, sends `kill -9`, restarts, and
   measures time to `/health/ready`. Then it reads everything back and checks
   that **every write the server acknowledged is present**, and runs
   `forgebase doctor`. Three kill cycles for records, one for queue messages.

## 3. SPEC targets

SPEC section 21 lists what to measure (throughput, p50/p95/p99, CPU, memory, disk
I/O, index size, recovery time) and says targets must be defined on documented
hardware. **It gives no numbers.** The targets below are therefore *proposed by
this benchmark run* and written down before they could change any code, so god
can accept or move them. Pass or miss is against these.

## 4. Results

### 4.1 In-process (Criterion), internal disk unless noted

| Area | Benchmark | Median | Throughput | Target | Result |
|---|---|---|---|---|---|
| DB writer | commit 1 row (durable) | 64 µs | 15.5 k tx/s | < 5 ms | Pass |
| DB writer | commit 100 rows in one tx | 552 µs | 181 k rows/s | n/a | n/a |
| DB reader | primary-key select | 21.6 µs | 46 k/s | < 1 ms | Pass |
| DB reader | `count(*)` over 10 k rows | 164 µs | 6.1 k/s | n/a | n/a |
| Records | create | 120 µs (79 µs in an earlier run) | 8.3 k/s | < 5 ms | Pass |
| Records | get by id | 30 µs | 33 k/s | < 1 ms | Pass |
| Records | list, indexed range, page of 20 (with total) | 130 µs | 7.7 k/s | < 5 ms | Pass |
| Records | same, `skip_total` | 76 µs | 13 k/s | n/a | n/a |
| Auth | access-token verify (signature + live session check) | 26.9 µs | 37 k/s | < 100 µs | Pass |
| Auth | password sign-in (Argon2id + session insert) | 15.3 ms | 65/s | < 250 ms | Pass |
| Policy | check, user with role | 368 ns | 2.7 M/s | < 10 µs | Pass |
| Policy | check, administrator | 10.9 ns | 92 M/s | n/a | n/a |
| KV | GET | 1.16 µs | 859 k/s | > 100 k/s | Pass |
| KV | SET | 1.69 µs | 591 k/s | > 100 k/s | Pass |
| KV | INCR | 0.60 µs | 1.67 M/s | n/a | n/a |
| Vector | top-10, HNSW, 10 000 x 128, cosine | 152 µs | 6.6 k/s | < 5 ms | Pass |
| Vector | top-10, exact (brute force), same data | 6.34 ms | 158/s | informational | n/a |
| Queue | publish 1 message | 166 µs | 6.0 k/s | < 5 ms | Pass |
| Queue | publish batch of 100 | 4.36 ms | 22.9 k msg/s | n/a | n/a |
| Queue | pull 10 + ack | 27.5 ms before FIX-9; 0.93 ms after (10 000-message backlog, internal disk) | 10.8 k msg/s | < 10 ms | Pass |
| Jobs | enqueue | 250 µs | 4.0 k/s | < 5 ms | Pass |
| Jobs | claim + complete (1 job) | 3.19 ms | 313/s | < 10 ms | Pass |

Storage sensitivity (same benches on the external disk, whose flushes stall):
DB commit 1 row 63 ms, records create 9.6 ms, queue publish 447 ms, jobs enqueue
131 ms, jobs claim + complete 522 ms. The server's own durability cost is small;
the device decides. Use a local SSD for production.

### 4.2 HTTP load (release server, 16 connections, internal disk)

| Scenario | Operation | Requests | Errors | Per second | p50 | p95 | p99 | Target (p99) | Result |
|---|---|---|---|---|---|---|---|---|---|
| Records | create | 21 882 | 0 | 2 187 | 2.18 ms | 3.35 ms | 4.76 ms | < 25 ms | Pass |
| Records | read | 21 882 | 0 | 2 187 | 0.10 ms | 0.21 ms | 0.30 ms | < 10 ms | Pass |
| Records | update | 21 882 | 0 | 2 187 | 2.12 ms | 3.31 ms | 4.63 ms | < 25 ms | Pass |
| Records | delete | 21 882 | 0 | 2 187 | 2.20 ms | 3.37 ms | 4.70 ms | < 25 ms | Pass |
| Records | list page of 20 (one connection) | 500 | 0 | 6 509 | 0.15 ms | 0.18 ms | 0.32 ms | < 10 ms | Pass |
| Queue | publish 1 | 22 974 | 0 | 2 297 | 2.83 ms | 10.7 ms | 19.7 ms | < 50 ms | Pass |
| Queue | publish 10 (batch) | 7 648 | 0 | 765 (7 648 msg/s) | 5.88 ms | 13.8 ms | 32.9 ms | < 50 ms | Pass |
| Queue | pull + ack 100 messages | 200 | 0 | 95 (9 552 msg/s) | 10.4 ms | 13.7 ms | 17.3 ms | < 50 ms | Pass |
| Realtime | fan-out, 30 subscribers | 35 970 events | 0 | 3 597 events/s in, 107 895 deliveries/s out | 1.21 ms | 1.49 ms | 3.42 ms | < 100 ms | Pass |
| Auth | sign-in accepted (`200`) | 26 | 0 | about 3 | 25 ms | 32 ms | 33 ms | < 250 ms | Pass |
| Auth | sign-in refused by the per-IP limiter (`429`) | 705 522 | 0 | 70 547 | 0.10 ms | 0.20 ms | 0.28 ms | n/a | n/a |

Realtime delivered 1 079 100 of 1 079 100 expected events: none lost.
Records throughput counts each of the four operations separately (so 8 748
requests per second overall). The first HTTP sign-in attempts per minute are
accepted, then the limiter answers `429` quickly; HTTP sign-in capacity is
therefore bounded by the limiter, and the Argon2id cost is the in-process 15 ms above.

The same records scenario on the external disk gave create p50 70 ms, p95 489 ms,
p99 589 ms at 36 operations per second, and an earlier run with 16 connections
and a hot limiter saw 22 s p50 while the disk stalled (flush p95 3.2 s, server log
shows 235 slow-query warnings for simple outbox reads). That is the disk, not the
server; see §1.

Resource use: the release server was at 82 MB resident memory after the full load
run (external-disk run, `ps`). CPU per scenario and disk usage were not sampled
in this run; `fb-load` now samples them (§9.1).

### 4.3 Recovery after `kill -9`

The test runs the debug-profile binary; 8 writers, 3 s of
writes per cycle, then SIGKILL, restart.

| Run | Cycle | Acknowledged writes | Rows after restart | Acknowledged but lost | Kill to ready |
|---|---|---|---|---|---|
| Internal disk, records | cold start | n/a | n/a | n/a | 538 ms (first start, migrations) |
| Internal disk, records | 1 | 9 248 | 9 248 | 0 | 542 ms |
| Internal disk, records | 2 | 9 661 (total 18 909) | 18 909 | 0 | 543 ms |
| Internal disk, records | 3 | 8 871 (total 27 780) | 27 781 | 0 | 530 ms |
| Internal disk, queue | 1 | 11 416 | 11 416 | 0 | 550 ms |
| External disk, records | 1 to 3 | 594 / 68 / 448 | all present | 0 | 2 642 / 801 / 1 417 ms |
| External disk, queue | 1 | 151 | 152 | 0 | 689 ms |

"Rows after restart" can exceed "acknowledged" by one: a write committed just
before the kill whose response never reached the client. The reverse (an
acknowledged write missing) never happened. The WAL at the kill was about 4 MB.
`forgebase doctor` passed after the last cycle in both runs.

| Recovery target | Value | Result |
|---|---|---|
| Time from kill to ready (internal disk) | 0.53 to 0.55 s | Pass (< 5 s) |
| Time from kill to ready (external disk, stalls) | 0.7 to 2.6 s | Pass (< 5 s) |
| Acknowledged writes lost | 0 of 27 780 records, 0 of 11 416 queue messages | Pass (must be 0) |

Not covered: power loss (needs a power cut; with `synchronous = FULL` on Linux
the commit is on stable storage, on macOS it is only in the OS cache), and KV
(by design its writes are in memory and flushed behind; acknowledged KV writes
inside the flush window are not durable and were not tested).

## 5. Pass or miss against SPEC section 21 measures

| SPEC measure | Measured | Target | Result |
|---|---|---|---|
| SQL reads | 21.6 µs point read in process | < 1 ms | Pass |
| SQL writes | 64 µs durable commit (macOS, internal disk) | < 5 ms | Pass (see §1 caveat) |
| Document reads | 0.10 ms p50, 0.30 ms p99 over HTTP | p99 < 10 ms | Pass |
| Document writes | 2.2 ms p50, 4.8 ms p99 over HTTP | p99 < 25 ms | Pass |
| Key-value operations | 859 k GET/s, 591 k SET/s | > 100 k/s | Pass |
| Vector queries | 152 µs HNSW top-10 over 10 k x 128 | < 5 ms | Pass |
| Realtime fan-out | 108 k deliveries/s to 30 subscribers, p99 3.4 ms, 0 lost | p99 < 100 ms | Pass |
| Authentication | token verify 27 µs; sign-in 15 ms | < 100 µs; < 250 ms | Pass |
| API throughput | 8.7 k requests/s mixed records CRUD on 16 connections | > 1 k/s | Pass |
| Recovery time | 0.55 s, 0 acknowledged writes lost | < 5 s; 0 lost | Pass |
| Queue pull + ack | 27.5 ms before FIX-9; 0.93 ms after, 10 messages in process | < 10 ms | Pass |
| Object uploads and downloads | benches and `fb-load` scenario exist (§9); Linux run pending | n/a | Not measured yet |
| Graph traversals | benches and `fb-load` scenario exist (§9); Linux run pending | n/a | Not measured yet |
| CPU, disk, memory, fds | sampled by `fb-load` since §9; Linux run pending | n/a | Not measured yet |

## 6. Misses and findings (for god; nothing was changed)

1. **Queue pull + ack (fixed in FIX-9, ADR-0025).** It was 27.5 ms for 10
   messages in process (publish 166 µs, jobs claim 3.2 ms). Cause: no fixed wait
   or timer (a pull with `wait_ms = 0` never sleeps, and no transaction is held
   across a wait). The claim query filtered on `(subscription, state,
   available_at)` and ordered by `seq`, so SQLite read every ready row, joined
   it to its message and sorted all of them (`USE TEMP B-TREE FOR ORDER BY`)
   inside the write transaction. The bench subscription held the whole backlog
   of the earlier publish benches, so the cost grew with it. Migration 0025 adds
   an index on `(subscription, state, seq)` and the query now reads only the rows
   it claims. Delivery guarantees and ordering are unchanged.

   Before and after, same bench (`cargo bench -p forgebase-bench --bench
   queue_jobs -- queue_pull`, internal disk, pull 10 + ack with a standing
   backlog, 10 publishes outside the timing per iteration):

   | Standing backlog | Before | After |
   |---|---|---|
   | about 2 000 messages | 1.00 ms | 0.99 ms |
   | about 20 000 messages | 2.21 ms | 0.93 ms |

   After the fix the time no longer depends on the backlog; the remaining 0.9 ms
   is the two commits (pull, ack). On the external disk the same bench is
   dominated by flush stalls (15 to 150 ms run to run), see §1. Guard:
   `pull_claim_query_does_not_sort_the_backlog` (queue tests) fails if the plan
   gains a sort or loses the index.
2. **Per-IP sign-in limiter caps HTTP sign-in.** Only 26 sign-ins per 10 s were
   accepted from one address; the rest are `429` (by design, ADR-0004). Also a
   tripped limiter makes the *administrator* login fail for the same address for
   up to a minute, so a load test from one host locks itself out of the admin
   login. `ip_attempts_per_minute` is not a config-file option. Worth knowing for
   anyone load-testing.
3. **Realtime per-principal cap: 32 connections** by default; since FIX-9 it is
   `realtime.max_connections_per_principal` in `forgebase.toml`. The fan-out test used 30 subscribers
   on one identity; a 100-subscriber run failed with `429 too_many_connections`.
4. **Write latency depends on the device** (§1, §4.1). A disk with stalls turns
   a 2 ms create into seconds. Nothing in the server was at fault in those runs.
5. During development of the bench, a list query appeared to take 11 to 18 ms
   with a full table scan. That was a benchmark mistake (the index was never
   created); with `notes_n` the plan uses the index (`PLAN indexes_used`) and the
   query takes 130 µs. No server defect.

## 7. Not done and limits

- Not measured in the run above: object uploads and downloads, graph
  traversals, the SQL endpoint, fuzzy search, CPU and disk over time, replicas.
  The tooling for all of them now exists (§9) and a Linux run produces the
  numbers; none have been recorded yet.
- Results are one run on a busy shared laptop; no repeated runs, no confidence
  intervals beyond Criterion's. Linux numbers are missing (§9).
- The recovery test uses the debug-profile binary. Recovery cost is dominated by
  WAL replay and migrations check, so release should be no slower.

## 8. Reproduce

```sh
export CARGO_INCREMENTAL=0 TMPDIR=/path/on/the/disk/under/test
cargo bench -p forgebase-bench                       # all Criterion benches
cargo build --release -p forgebase-cli -p forgebase-bench --bins
crates/forgebase-bench/run-load.sh target/release/forgebase target/release/fb-load /path/work 16 10
cargo test -p forgebase-cli --test recovery -- --nocapture --test-threads=1
```

On Linux, with both backends, the new benches and the JSON and resource output:
§9 (`.github/workflows/bench.yml`).

## 9. Linux scale validation (the `bench` workflow)

Everything above was measured on one shared Mac laptop and covers SQLite only.
This section is the method for the Linux numbers and what was added to produce
them. **No Linux result is recorded in this document yet**: the workflow exists
and its pieces were smoke-run, but a real run needs a Linux runner (see §9.6).
Where a number is needed before then, [capacity-planning.md](capacity-planning.md)
quotes §4 above and labels it "measured on M4 laptop; Linux TBD".

### 9.1 What changed

| Area | Before | Now |
|---|---|---|
| Criterion benches | `db`, `records`, `auth_policy`, `kv`, `vector`, `queue_jobs` | adds `storage`, `graph`, `sql`, `fuzzy` (below); all of them run on SQLite and on the bundled PostgreSQL (`FORGEBASE_TEST_BACKEND=postgres`) |
| `fb-load` scenarios | `records`, `auth`, `queue`, `realtime` | adds `storage`, `graph`, `sql`, and the two topology scenarios `topology-setup` and `topology` |
| Server resources | resident memory by hand with `ps` | `--pid`/`--data-dir`: CPU %, RSS, open fds and data-directory disk usage of the server **process tree** (the bundled PostgreSQL included) once a second, per scenario, as p50 and max |
| Output | `RESULT` lines on stdout | the same lines, and `--json <file>`: one document per run (format 1, `crates/forgebase-bench/src/report.rs`) that CI stores and compares |
| CI | none for benchmarks | `.github/workflows/bench.yml` (§9.3) |
| Comparison | by eye | `crates/forgebase-bench/scripts/bench_report.py` against `baseline/linux.json` (§9.4) |

The new Criterion benches, all in process with real persistence (a temp
directory, production pragmas and migrations):

- **`storage`** (local driver): put and get of 4 KiB, 1 MiB and 16 MiB objects
  (each put has unique content, so deduplication does not hide the write), a
  multipart upload of four 4 MiB parts, and listing (a delimiter at the root,
  a prefix page of 100, paging through one folder by cursor) over 5 000
  objects.
- **`graph`**: 10 000 nodes and 50 000 edges (a ring plus pseudo-random `KNOWS`
  edges from a fixed seed, bulk-loaded). Traversal at depth 1 to 4, neighbours,
  shortest path (max depth 8) and openCypher on the portable engine (ADR-0034).
  On PostgreSQL with Apache AGE installed it then materialises the derived
  graph and runs the same three queries again as `cypher_age_*`; without AGE
  that half is skipped and says so.
- **`sql`**: the SQL editor's data layer (`DataService::execute_sql`, the code
  behind `POST /api/v1/sql`) on `customers` (10 000 rows) and `orders` (50 000
  rows): point lookup by primary key and by secondary index, an indexed range
  scan, an aggregate over the whole table and over one customer, a join of the
  two tables, a single insert and a batch of 100.
- **`fuzzy`**: a `fuzzy` list query on a trigram index (portable engine) at
  10 000 and at 100 000 rows, a one-word typo and a two-word query, plus an
  insert with trigram maintenance. The bench prints the engine in use.

`cargo bench` runs them at full scale. `cargo test -p forgebase-bench --benches`
runs each once at a small scale (a few hundred rows; `FB_BENCH_SCALE=full|smoke`
overrides), which is how they are checked on a machine that must not do a
release build.

### 9.2 Method on Linux, and the disk-flush caveat

- Runner: `ubuntu-24.04` (GitHub-hosted: 4 vCPU, 16 GiB, one shared SSD, no
  isolation from neighbours). Release build, the same profile as §1. A
  self-hosted runner gives steadier numbers; the workflow does not care.
- Both backends, as two matrix jobs. PostgreSQL uses the engine `engine.lock`
  pins, downloaded and verified like the other workflows do
  (`FORGEBASE_PG_ENGINE_DIR`).
- The job records the machine and a **fsync probe** (300 times 4 KiB write plus
  `fsync` in the work directory, p50, p95 and max) next to the results, so a
  number can be read against the disk it came from.
- Scale is full: the data sets above, 16 connections and 30 s per `fb-load`
  scenario by default (workflow inputs `duration` and `concurrency`).
- Client and server share the machine, as in §1. HTTP numbers are a lower
  bound for the server.

**Disk-flush caveat: Linux versus macOS.** This is the biggest reason the Mac
write numbers do not carry over.

| | macOS (the §4 numbers) | Linux (this workflow) |
|---|---|---|
| What SQLite's `synchronous = FULL` calls | plain `fsync`, which does **not** flush the drive's write cache | `fsync`, which does: the commit waits for the device |
| Cost of one durable commit | about 65 µs (internal SSD) | the device's flush latency: typically hundreds of µs on a local NVMe, milliseconds on network-attached or virtualised disks |
| What it makes slow | nothing visible | every single-row commit: `db/writer_commit_1_row`, `records/create`, `sql/insert_1`, queue publish, jobs enqueue, `fb-load` `records` create/update/delete |
| What it does not touch | | reads, KV, vector search, auth verify, policy checks, traversals |
| PostgreSQL | `fsync` too, so the same cheap flush | `fsync` and WAL flush at each commit (`synchronous_commit = on`): the same device cost per commit, plus WAL volume |

Consequences when reading results: (1) write-bound rows will be far slower than
§4.1 and that is the honest figure, not a regression; (2) batching (100 rows in
one transaction) amortises the flush, so compare `commit_1_row` with
`commit_100_rows` to see how much of a number is the device; (3) a GitHub
runner's disk is shared and virtual, so its flush latency varies from run to
run and the fsync probe line is the first place to look when a write row moves;
(4) never compare a macOS write number with a Linux one. Power-loss durability
is only true of the Linux figures (§4.3).

### 9.3 The CI job

`.github/workflows/bench.yml` runs on `workflow_dispatch` (inputs: `duration`,
`concurrency`, `topology`) and weekly (Sunday 04:21 UTC). Not on pull
requests: a full run takes about an hour per backend.

| Job | Steps | Artifacts (`bench-sqlite`, `bench-postgres`, `bench-topology`) |
|---|---|---|
| `bench (sqlite)` and `bench (postgres)` | build release; machine and fsync probe; `cargo bench -p forgebase-bench --benches` at full scale; `fb-load` (`records`, `queue`, `realtime`, `auth`, `storage`, `graph`, `sql`) against a release server through `run-load.sh`; comparison | `criterion-<backend>.json` (medians), `criterion-<backend>-data.tar.gz` (the whole `target/criterion` tree), `fb-load-<backend>.json` and `.log`, `system-<backend>.txt`, `comparison-<backend>.md`, `server-<backend>.log` |
| `topology load` (PostgreSQL) | the topology scenario below | `fb-load-topology.json` and `.log`, `server-topology.log`; the lag and read-distribution tables are also in the step summary |

**Topology scenario** (`run-topology.sh`, then `fb-load --scenario topology`):
a bundled-PostgreSQL primary with two read replicas and two shards, six tenants
spread over the shards, driven through the public API.

1. `run-topology.sh` starts the server, then `fb-load --scenario
   topology-setup` adds the replicas (`POST /api/system/database/nodes`,
   `kind: local`) and the shards (separate PostgreSQL clusters started with the
   engine's `initdb` and `pg_ctl`, added with `POST /api/system/database/shards`)
   and waits for every operation and for the replicas to enter the read
   rotation. The server takes replicas up when it opens its router, so the
   script restarts it.
2. `fb-load --scenario topology` runs writers (30 % of the connections by
   default) and readers for the duration. Writers `POST` records into a
   tenant-scoped collection (each request names one of the tenants, so the
   writes spread over the shards). Readers rotate between a list at eventual
   consistency, a list with the latest commit token (`X-ForgeBase-Read-After`,
   read-your-writes) and a SQL probe with `read_from_replica` that returns the
   node it ran on.
3. Once a second a sampler reads `GET /api/system/database/topology`: each
   replica's `lag_ms`, `lag_bytes`, state and rotation.

The report (`extra.topology` in the JSON, `RESULT scenario=topology` lines) has
the latency of each operation, per-replica lag (p50, p95, max, and seconds out
of rotation), the share of probes each node served (`primary`, `replica-1`,
...: named in order of the postmaster start time, not the topology names),
`replica_fallbacks_delta` (the `forgebase_db_replica_fallbacks_total` counter
before and after: reads that fell back to the primary) and the placement of the
tenants.

Reading the topology numbers:

- The API holds back a read until a replica has replayed the caller's own
  writes (ADR-0029 §2.6), so a client never *sees* replica lag. The lag figures
  are the replicas' apply lag, and what lag costs you is a lower replica share
  and more fallbacks. A healthy run has lag in the low milliseconds and a share
  close to the replicas' weight.
- Primary, replicas and shards share one 4-core machine: the run measures the
  routing, the replication stream and the lag under load, **not** the extra
  capacity separate hosts would add. Expect write throughput to be lower than a
  single primary alone (the replicas replay it on the same cores).
- `wal_level = logical` costs about 1.4 % more WAL (ADR-0029 addendum).

Run it by hand: `crates/forgebase-bench/run-topology.sh <forgebase> <fb-load>
<empty dir> [connections] [seconds] [replicas] [shards] [tenants]`. It needs
`FORGEBASE_PG_ENGINE_DIR` for the shards (and for a binary built without the
embedded engine).

### 9.4 Reading and comparing results

**The JSON** (`fb-load --json`, format 1):

```json
{
  "format": 1, "tool": "fb-load", "label": "linux-postgres", "backend": "postgres",
  "host": {"os": "linux", "arch": "x86_64", "cpus": 4},
  "config": {"concurrency": 16, "duration_secs": 30},
  "results": [{"scenario": "records", "op": "create", "n": 1, "errors": 0, "rps": 1,
               "p50_us": 1, "p95_us": 1, "p99_us": 1}],
  "resources": [{"scenario": "records", "samples": 30,
                 "cpu_pct": {"p50": 1, "max": 1}, "rss_mb": {"p50": 1, "max": 1},
                 "open_fds": {"p50": 1, "max": 1},
                 "disk_mb": {"start": 1, "p50": 1, "max": 1, "end": 1}, "procs_max": 1}],
  "extra": {"topology": {}}
}
```

Rows are keyed `scenario/op`. Storage rows also carry `mib_per_s`; graph, SQL
and topology rows carry the data size. CPU is a percentage of **one core** (200
is two busy cores) over the process tree, so a PostgreSQL run counts the
engine's processes too. Open descriptors come from `/proc` on Linux; off Linux
they are a best-effort `lsof` count that includes memory maps and overstates.
Rows with errors are not used for comparisons.

**Criterion** values are the median, as in §2, in `criterion-<backend>.json`
(`benches["<group>/<bench>"].median_ns`); the full data is in the tar.

**Comparing.** `bench_report.py compare` reads one or more run files and the
baseline and prints a table of metrics more than 20 % worse (and better); for
each regression it prints a GitHub `::warning` annotation and it appends the
table to the step summary. It **never fails** the job: shared runners are noisy
and an unrecorded baseline must not turn a run red. Metrics compared: Criterion
median (lower is better), `rps` (higher), `p95_us` and `p99_us` (lower), and
per scenario `rss_mb.max` and `cpu_pct.p50` (lower). Latency changes smaller
than 50 µs are ignored (`--noise-floor-us`).

**The baseline** is `crates/forgebase-bench/baseline/linux.json`. Its format
(documented in `bench_report.py`) is committed with every value `null` and a
note: **TBD, no Linux run is recorded.** A `null` is "not recorded": it is
listed as unbaselined and never warns. To record one:

1. Run the workflow (Actions, bench, Run workflow) on a commit you want as the
   reference, with the default `duration` and `concurrency`.
2. Download `bench-sqlite`, `bench-postgres` and `bench-topology`.
3. `python3 crates/forgebase-bench/scripts/bench_report.py baseline --out
   crates/forgebase-bench/baseline/linux.json --runner "ubuntu-24.04 (GitHub-hosted, 4 vCPU)"
   --commit <sha> --recorded-at <date> criterion-*.json fb-load-*.json` and
   commit the file.
4. Re-record after a deliberate performance change, or when the runner type
   changes; keep the old file in history. One run is a weak baseline: record
   from the median of three runs when the numbers will gate decisions.

To compare two runs of your own (a branch against `main`), record the first
with `baseline --out a.json` and run `compare --baseline a.json` on the second.

### 9.5 Local smoke runs (not benchmarks)

```sh
export CARGO_INCREMENTAL=0 TMPDIR=/path/on/the/disk/under/test
cargo test -p forgebase-bench --benches                       # every bench once, small data, SQLite
FORGEBASE_TEST_BACKEND=postgres FORGEBASE_PG_ENGINE_DIR=<engine> \
  cargo test -p forgebase-bench --benches                     # the same on PostgreSQL
cargo build -p forgebase-cli -p forgebase-bench               # debug binaries
SCENARIOS=storage,graph,sql JSON_OUT=/tmp/smoke.json \
  LOAD_ARGS="--graph-nodes 300 --graph-edges 1500 --sql-customers 200 --sql-orders 1000 --storage-large-mib 2" \
  crates/forgebase-bench/run-load.sh target/debug/forgebase target/debug/fb-load /tmp/work 4 3 sqlite
crates/forgebase-bench/run-topology.sh target/debug/forgebase target/debug/fb-load /tmp/topo 6 8 2 1 3
python3 -m unittest discover -s crates/forgebase-bench/scripts -p 'test_*.py'
```

These check that the code paths work (no errors in any scenario; replicas and a
shard come up and receive reads and tenants). The figures they print come from
a debug build on a busy laptop: do not quote them.

### 9.6 What still needs a real Linux run

- Every number in the new benches and scenarios (storage, graph, SQL, fuzzy,
  resource use, topology). The code was checked to compile and to run clean on
  macOS with SQLite and PostgreSQL; nothing was timed in a release build.
- The AGE half of the `graph` bench has not run anywhere yet: the macOS test
  engine has no AGE. The pinned engine has it, so the Linux PostgreSQL job will
  show the portable-versus-AGE comparison, or skip it with a message if the
  derived graph cannot be set up from the bench's database handle.
- `bench.yml` passed `actionlint`; it has not run on GitHub.
- Replica lag and read distribution on separate hosts, and a network between
  primary and replica, are not covered by any job.
- The baseline (§9.4).
