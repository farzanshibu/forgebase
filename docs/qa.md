# ForgeBase final QA (FB-85)

This page records the final acceptance run of SPEC §27: the 25 minimum
scenarios, end to end, on a clean build. It says how to repeat the run, what
the machine was, and what each scenario proves.

## 1. What "end to end" means here

One harness, [`tests/integration/spec_scenarios.rs`](../tests/integration/spec_scenarios.rs),
attached to the `forgebase-cli` package as the test target `spec_scenarios`.
Every test:

1. creates an empty temporary directory and runs `forgebase init app` with the
   binary cargo just built (`CARGO_BIN_EXE_forgebase`);
2. creates the administrator with `forgebase admin create --password-stdin`;
3. starts `forgebase serve --port 0` as a child process and reads the bound
   address from its `listening on http://…` line;
4. talks to it only over sockets: HTTP/1.1 for REST, WebSocket and
   Server-Sent Events (SSE) for realtime, the CLI for backup and restore;
5. checks persistence by stopping the process with `SIGTERM` and starting it
   again on the same data directory.

The child processes get an empty `HOME` inside the temporary directory and no
`FORGEBASE_*` variables from the caller, so no state outside the test directory
is read or written. Scenario 20 sets `FORGEBASE_BILLING_ENABLED=true` for its
own server. No test reaches the internet: billing uses the `manual` provider
for collection and a locally signed Stripe webhook.

## 2. How to repeat the run

Clean machine run (fresh clone, empty target directory, no `~/.forgebase`):

```sh
export TMPDIR=/path/to/tmp CARGO_INCREMENTAL=0
git clone --branch <branch> --single-branch <repository> qa-clone
cd qa-clone
export CARGO_TARGET_DIR="$PWD/target"          # empty
cargo build --offline -p forgebase-cli --bin forgebase
cargo test  --offline -p forgebase-cli --test spec_scenarios -- --test-threads=6   # run A
cargo test  --offline -p forgebase-cli --test spec_scenarios -- --test-threads=6   # run B
```

`--offline` only means the crates come from the local cargo registry cache
(the sandbox has no registry access); drop it on a machine with network.
`--test-threads=6` bounds the number of servers running at once; the suite
also passes with the default thread count. One scenario alone:
`cargo test -p forgebase-cli --test spec_scenarios scenario_17`.

Checks that ran on the same commit: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`.

## 3. Environment

| Item | Value |
|---|---|
| Date | 2026-11-10 (UTC) |
| Commit | `dac846b` on `agent/worker-fb85`: `main` at `3a0866e` (includes FIX-5) plus the FB-85 harness and the `forgebase-db` fix below. FIX-9 had not landed. |
| Machine | Apple silicon (arm64), 10 cores, 16 GiB RAM, macOS 27.0.1 (26A434) |
| Toolchain | rustc 1.99.0 (b940084d7 2026-09-28), cargo 1.99.0; workspace `rust-version` 1.97 |
| Build | Debug profile, `CARGO_INCREMENTAL=0`, fresh `git clone` into an empty directory with an empty `CARGO_TARGET_DIR`: 335 crates compiled from scratch, no compiler cache, 2 min 39 s; target directory 2.0 GB |
| State | No `~/.forgebase`; child processes get an empty `HOME`; no `FORGEBASE_*` variables; every test has its own temporary data directory |
| SQLite | bundled through `libsqlite3-sys` 0.37.0 |
| Runs | A: 25 passed, 0 failed, 39.5 s. B: 25 passed, 0 failed, 55.2 s (`--test-threads=6`) |
| Gates on this commit | `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test -p forgebase-db -p forgebase-data -p forgebase-api -p forgebase-cli --no-fail-fast` 164 passed, 0 failed |

## 4. Results

| # | SPEC §27 scenario | Test (`spec_scenarios`) | Run A | Run B | What it checks | Also covered by |
|---|---|---|---|---|---|---|
| 1 | Create a project | `scenario_01_create_a_project` | pass | pass | `init` creates config and database; a user creates an organization and a project with its three environments; duplicate slug `409`; anonymous `401`; project read back after a restart | `crates/forgebase-api/tests/policy_http.rs`, `crates/forgebase-policy/tests/isolation.rs` |
| 2 | Create a SQL table | `scenario_02_create_a_sql_table` | pass | pass | `CREATE TABLE` through `/api/v1/sql`, a parameterised batch, a failing batch rolled back whole, table introspection, rows after a restart | `crates/forgebase-api/tests/scenarios.rs`, `crates/forgebase-data/tests/scenarios.rs` |
| 3 | Create a document collection | `scenario_03_create_a_document_collection` | pass | pass | Flexible schema with a required field, nested JSON, merge patch with `If-Match`, stale version `409 version_conflict`, document after a restart | same |
| 4 | Insert and query records | `scenario_04_insert_and_query_records` | pass | pass | Batch and single insert, filter with bound `p.*` parameters, sort, page size, projection, cursor pagination over all 61 rows, JSON query form, type validation with nothing written | same |
| 5 | Create a composite index | `scenario_05_create_a_composite_index` | pass | pass | Two-key index (one descending) with its columns, duplicate name `409`, unknown field refused, definition after a restart | same |
| 6 | Validate index usage with query plans | `scenario_06_validate_index_usage_with_query_plans` | pass | pass | `explain` shows a full scan before and the composite index after; the same query returns the right rows; raw SQL `explain` uses the index | same |
| 7 | Authenticate a user | `scenario_07_authenticate_a_user` | pass | pass | Sign-up, case-insensitive sign-in, `/api/auth/me`, administrator sign-in, session survives a restart, refresh rotation, reuse of a rotated refresh token ends the session, sign-out revokes at once | `tests/integration/auth_e2e.rs`, `crates/forgebase-auth/tests/auth_flows.rs` |
| 8 | Reject unauthorized requests | `scenario_08_reject_unauthorized_requests` | pass | pass | No credential `401 unauthenticated` with a request id, garbage and `alg=none` and altered-signature tokens `401`, wrong password and unknown user `401`, a user on five administrator routes `403`, nothing changed | same |
| 9 | Verify tenant isolation | `scenario_09_verify_tenant_isolation` | pass | pass | Tenant-scoped collection, two organizations: another tenant's header `403` at step `tenant`; a known id from another tenant `404` for read, update and delete; each list holds only its own records | `crates/forgebase-api/tests/policy_http.rs` |
| 10 | Upload and retrieve an object | `scenario_10_upload_and_retrieve_an_object` | pass | pass | 300 kB upload with SHA-256 checksum and metadata, wrong checksum refused, bytes, ETag, metadata and a byte range after a restart, listing, anonymous `401`, delete then `404` | `crates/forgebase-storage/tests/storage.rs` |
| 11 | Generate a signed download URL | `scenario_11_generate_a_signed_download_url` | pass | pass | Signed URL works with no credentials and sets the download name; plain path `401`; altered signature or expiry refused; a 1-second URL stops working | same |
| 12 | Store and retrieve a vector | `scenario_12_store_and_retrieve_a_vector` | pass | pass | Upsert 50 vectors with metadata, read one back, dimension mismatch `400`, item and count after a restart | `crates/forgebase-vector/tests/http.rs`, `crates/forgebase-server/tests/vector.rs` |
| 13 | Execute vector similarity search | `scenario_13_execute_vector_similarity_search` | pass | pass | 200 vectors through the ANN index (`exact_threshold` 0): nearest neighbours in order, metadata filter, wrong dimension `400` | same |
| 14 | Create graph nodes and edges | `scenario_14_create_graph_nodes_and_edges` | pass | pass | Nodes and typed edges, edge to a missing node refused, duplicate id `409`, data and stats after a restart, deleting a node removes its edges | `crates/forgebase-graph/tests/graph.rs` |
| 15 | Execute graph traversal | `scenario_15_execute_graph_traversal` | pass | pass | Bounded traversal by edge type with depths, outgoing neighbours, shortest path a→b→c→d, injection in the start id `404` with nothing changed | same |
| 16 | Subscribe to realtime events | `scenario_16_subscribe_to_realtime_events` | pass | pass | WebSocket welcome and subscription, connection counted in stats, a user without read access is refused per subscription, SSE stream opens with the subscription acknowledged | `tests/integration/realtime_e2e.rs`, `crates/forgebase-realtime/tests/realtime.rs` |
| 17 | Verify event delivery after a committed change | `scenario_17_verify_event_delivery_after_a_committed_change` | pass | pass | A rolled-back batch emits nothing; created, updated, deleted arrive in order over WebSocket and SSE; the record is readable when its event arrives; after a restart the outbox replays all three from a cursor | same |
| 18 | Execute a sandboxed server function | `scenario_18_execute_a_sandboxed_server_function` | pass | pass | Users cannot deploy; WebAssembly deployed over REST and invoked anonymously; execution logged; an infinite loop stopped by fuel (`502 function_fuel_exhausted`) in under 15 s; a WASI file import refused at deploy; a host call without its capability refused | `tests/integration/functions_e2e.rs`, `crates/forgebase-functions/tests/{http,sandbox}.rs` |
| 19 | Schedule and execute a background job | `scenario_19_schedule_and_execute_a_background_job` | pass | pass | Delayed `function.invoke` job with idempotency key (repeat returns the same job), not run early, succeeds once with the function's response, events `enqueued, claimed, succeeded`; a per-second cron schedule runs, survives a restart and keeps firing | `crates/forgebase-jobs/tests/http.rs`, `crates/forgebase-server/tests/jobs.rs` |
| 20 | Process a billing webhook idempotently | `scenario_20_process_a_billing_webhook_idempotently` | pass | pass | Billing enabled by environment; plan, product, price, customer (manual collection), subscription with an open invoice; forged signature `400 invalid_signature`; signed event `processed`, redelivery `duplicate`; invoice paid once, one payment, subscription active; after a restart still `duplicate`, one event with 3 attempts | `crates/forgebase-server/tests/billing.rs` |
| 21 | Restart the server and verify persistence | `scenario_21_restart_the_server_and_verify_persistence` | pass | pass | After one restart: a record, a user session, an object, vectors, graph edges, a KV string and a deployed function all answer | `tests/integration/backup_e2e.rs::scenario_21_restart_keeps_collections_and_records` |
| 22 | Back up and restore the database | `scenario_22_back_up_and_restore_the_database` | pass | pass | `forgebase backup` while serving, `backup verify`; a later write; restore refused while serving; database destroyed, storage and indexes deleted; `restore latest --force`, `doctor` healthy; 25 records (not the later one), the object and the vectors back | `tests/integration/backup_e2e.rs::scenario_22_online_backup_then_restore_after_data_loss` |
| 23 | Rebuild derived indexes | `scenario_23_rebuild_derived_indexes` | pass | pass | Online vector rebuild (`202`, reason `manual`) and data index rebuild keep answers identical; vector index files deleted with the server stopped are rebuilt on start (reason `missing`, 200 vectors) with the same results | `crates/forgebase-server/tests/vector.rs` |
| 24 | Test concurrent read and write workloads | `scenario_24_concurrent_read_and_write_workloads` | pass | pass | 8 writers × 40 inserts and 4 readers in parallel over HTTP: no errors, counts never go back, 320 distinct ids, per-writer counts exact; 8 racing updates with the same version: exactly one `200`, seven `409` | `crates/forgebase-data/tests/scenarios.rs::scenario_24_concurrent_reads_and_writes` |
| 25 | Test malicious inputs and permission bypass attempts | `scenario_25_malicious_inputs_and_permission_bypass_attempts` | pass | pass | Injection through filter and sort, multi-statement and system-table SQL, `ATTACH`; fail-closed collection for users; role and verified escalation through a profile update; `..` object keys stay opaque (no file outside the blob store); HTML declared as PNG refused; broken JSON `400`, wrong media type `415`, 40 MiB body refused; request id sanitised; data intact and ready | `crates/forgebase-server/tests/security_bypass.rs` (FB-80), `crates/forgebase-data/tests/security.rs` |

**25 of 25 pass, twice in a row, on the clean build.**

## 5. Failures found and fixed

| Scenario | Symptom on the binary | Root cause | Fix |
|---|---|---|---|
| #5, #6 | Right after `POST /api/v1/indexes`, the index came back with `"columns": []`, and `explain` kept reporting `SCAN` with `indexes_used: []`, although SQLite had the index. The in-process tests passed because their single reader reloaded the schema by chance. | `forgebase-db` refreshes pooled read connections only after a `TxMode::SchemaChange` commit (MDF-52). The index manager runs `CREATE INDEX` in a `TxMode::Normal` transaction, so the schema generation was not bumped. A reader that had already loaded the schema answered `EXPLAIN QUERY PLAN` and `PRAGMA index_info` from its stale in-memory copy (SQLite builds both at prepare time without checking the schema cookie). | `SqliteTx` marks a transaction that executes `CREATE`, `DROP`, `ALTER`, `ANALYZE` or `REINDEX`, and its commit bumps the generation like a schema change ([`sqlite_backend.rs`](../crates/forgebase-db/src/sqlite_backend.rs)). Regression test `readers_see_an_index_created_in_a_normal_transaction` in [`crates/forgebase-db/tests/stale_statements.rs`](../crates/forgebase-db/tests/stale_statements.rs) fails without the fix and passes with it. |

Other first-run failures were wrong expectations in the new harness, corrected
against the code and the documented design: a `function.invoke` job's result is
the function's whole response (`{"status", "body"}`); graph traversal and
neighbour items wrap the node (`{"depth", "node"}`, `{"direction", "edge",
"node"}`); object keys containing `..` are opaque names stored as given,
never paths (docs/security.md, path traversal row), so the test checks that no
file appears outside the blob store instead of expecting a refusal; a request id
with a control character makes the HTTP layer reject the request, so the test
sends a printable but disallowed id.

## 6. Limits of this run

- One machine, one operating system (macOS on Apple silicon). CI
  (`.github/workflows/ci.yml`) runs the test suite on Ubuntu 24.04; this run
  was not repeated on Linux or Windows.
- Debug builds (the team rule on this disk is no `--release`); timings in the
  table are debug timings.
- SPEC §27 #25 here is the black-box part. SSRF, the outbound address guard,
  scope bypass with API keys and the rest of the threat model are covered by the
  in-process FB-80 suite,
  [`crates/forgebase-server/tests/security_bypass.rs`](../crates/forgebase-server/tests/security_bypass.rs),
  which runs against a real server on loopback but not the built binary.
- Push subscriptions to external URLs, real payment providers, S3 and SMTP are
  not exercised (no outbound network on a clean machine).
