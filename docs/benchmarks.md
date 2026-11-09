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

Three tools, all in the new dev-only crate `crates/forgebase-bench` (nothing in
the server depends on it; the only new dependency is `criterion`, dev-only,
MIT/Apache-2.0).

1. **Criterion benches** (`cargo bench -p forgebase-bench`), in-process, real
   SQLite in a temp directory, production pragmas and migrations. One file per
   area: `db`, `records`, `auth_policy`, `kv`, `vector`, `queue_jobs`
   (queue and jobs run through `forgebase_server::start`, the real lifecycle).
   Values below are the Criterion median (middle of the three numbers). Run with
   `TMPDIR=<dir on the disk under test>`.
2. **HTTP load harness** `fb-load` (`crates/forgebase-bench/src/bin/fb-load.rs`,
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
| Queue | pull 10 + ack | 27.5 ms | 363 msg/s | < 10 ms | **Miss** |
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
run (external-disk run, `ps`). CPU per scenario and disk I/O were not sampled;
see §7.

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
| Queue pull + ack | 27.5 ms for 10 messages in process | < 10 ms | **Miss** |
| Object uploads and downloads | not measured | n/a | Not measured |
| Graph traversals | not measured | n/a | Not measured |
| CPU, disk I/O, index size | not sampled | n/a | Not measured |

## 6. Misses and findings (for god; nothing was changed)

1. **Queue pull + ack is the slow queue path: 27.5 ms for 10 messages
   in process** (publish is 166 µs, jobs claim 3.2 ms). Over HTTP the batch of
   100 takes 10.4 ms p50, so the cost is mostly per call, not per message.
   Profile: the sum of two transactions (pull marks deliveries, ack deletes)
   cannot explain 27 ms at a 65 µs commit cost, so look first for a fixed wait
   or timer in the pull path and for the cost that grows with the backlog (the
   bench subscription held the whole backlog of earlier publishes). Not
   investigated further, as instructed.
2. **Per-IP sign-in limiter caps HTTP sign-in.** Only 26 sign-ins per 10 s were
   accepted from one address; the rest are `429` (by design, ADR-0004). Also a
   tripped limiter makes the *administrator* login fail for the same address for
   up to a minute, so a load test from one host locks itself out of the admin
   login. `ip_attempts_per_minute` is not a config-file option. Worth knowing for
   anyone load-testing.
3. **Realtime per-principal cap: 32 connections** (`max_connections_per_principal`),
   not configurable from `forgebase.toml`. The fan-out test used 30 subscribers
   on one identity; a 100-subscriber run failed with `429 too_many_connections`.
4. **Write latency depends on the device** (§1, §4.1). A disk with stalls turns
   a 2 ms create into seconds. Nothing in the server was at fault in those runs.
5. During development of the bench, a list query appeared to take 11 to 18 ms
   with a full table scan. That was a benchmark mistake (the index was never
   created); with `notes_n` the plan uses the index (`PLAN indexes_used`) and the
   query takes 130 µs. No server defect.

## 7. Not done and limits

- Not measured: object uploads and downloads, graph traversals, SQL endpoint,
  CPU and disk I/O over time, index size on disk. The harness has room for them
  (`fb-load` scenarios are small functions) but they were out of the time given.
- Results are one run on a busy shared laptop; no repeated runs, no confidence
  intervals beyond Criterion's. Linux numbers are missing.
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
