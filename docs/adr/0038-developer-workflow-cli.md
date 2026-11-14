# ADR-0038: Developer workflow (`forgebase dev`, `forgebase gen types`, `functions invoke`)

- Status: accepted
- Date: 2026-11-14
- Scope: `crates/forgebase-cli` (`src/dev/**`, `src/gen/**`, `src/functions.rs`, `src/testserver.rs`,
  `src/probe.rs`, `tests/dev_e2e.rs`). No migration, no server change, no new dependency.
- Builds on: ADR-0037 (the project directory, plan and apply, `seed/`), ADR-0035 (trace context on function
  logs and responses), ADR-0036 (per-invocation function profiles), ADR-0017 / ADR-0027 (functions), ADR-0028
  (both database backends).
- Reference for users: [docs/dev-workflow.md](../dev-workflow.md).

## Context

ADR-0037 made the server's configuration a directory of files and gave it a `seed/` directory "for the dev
emulator". What an app developer still lacks is the inner loop: a server they can start in a second, point
the project at, fill with fixtures, throw away, and start from a test suite; types for the collections they
just declared; and a way to see what a function did. This ADR adds those three things to the `forgebase`
binary and adds no server code.

## Decisions

### 1. `forgebase dev` runs the real server in-process

`forgebase dev` starts `forgebase_server::start_loaded` in the CLI process, on a data directory of its own and
its own configuration file, so a `forgebase.toml` or `forgebase-data/` in the working directory is never
touched or read. It is the same server as `forgebase serve` (same SQLite or bundled PostgreSQL engine, same
migrations, same routes); there is no second implementation and nothing to drift.

- **Data.** An ephemeral directory under the temp directory (`fbdev-<random>`), deleted when the emulator
  stops. `--persist DIR` keeps the data (and the generated credentials, `dev-credentials.json`, mode 0600) in
  `DIR`; restarting with the same `DIR` signs in as the same administrator and reuses the same API key. A
  persisted directory keeps its engine; `--database sqlite|postgres` chooses it for a new one, else
  `FORGEBASE_DATABASE_BACKEND`, else the build's default (as `forgebase init`).
- **Bootstrap.** The first-run setup (`/api/setup/admin`, FB-90) creates the administrator
  `dev@forgebase.local` with a random password; an `admin`-role service account `forgebase-dev` gets an API
  key `fbk_...`. Both are printed, and in `--json` mode are in the ready line. They are generated per
  directory, never fixed, and exist only in an emulator that listens on loopback unless `--host` says
  otherwise.
- **Port.** The global `--port` (default 8090; `0` picks a free port).
- **Shutdown.** Ctrl-C or SIGTERM: stop accepting requests, drain, close the database (and the bundled
  engine), delete the ephemeral directory, remove the registry entry, exit 0.
- **Environment.** `dev` never reads `FORGEBASE_URL` / `FORGEBASE_TOKEN`: the app's `.env` usually points
  them at a real server, and an emulator that acted on them could change it. `dev seed|reset` take
  `--server` and `--token` explicitly, and `reset` refuses a non-loopback server.
- **Trace context.** The server ties trace context (and so `traceparent` and `trace_id` on function logs,
  ADR-0035) to `[observability.otlp] enabled`. `dev` turns it on without exporting anything, unless the
  developer configured an exporter, so `functions invoke` can always show a trace id.

### 2. Applying the project

On start `dev` loads and validates the project (every problem with `file:line`, before anything starts), runs
the functions' `build` commands, signs in as the administrator and runs the ADR-0037 plan and apply
in-process: no prompt, **`--prune` on** (the emulator mirrors the project: removing a file removes the
resource) and **destructive changes allowed** (the data of an emulator is disposable). A plan with
destructive changes is rendered as usual and followed by a line saying they are allowed. A failed apply
stops a plain `dev` with exit code 1; with `--watch` the emulator keeps running and the next save retries.
Function `[secrets]` read environment variables as in `apply`; where a variable is not set the emulator
gives the secret the value `forgebase-dev-placeholder` (and warns), because a template that needs a Slack URL
must still start: `apply::Options` gained `placeholder_secret`, which `apply` and `deploy` leave unset.

A missing project directory starts an empty emulator with a warning; `--seed` and `--watch` need a project.

### 3. Seeding

`--seed` (and `forgebase dev seed`) load `seed/<collection>.jsonl` through `read_seed()` (ADR-0037 §2):

- **Idempotent by record id.** A line's `id` is the record's id. A line without one gets
  `seed-<16 hex>`, a digest of the collection and the canonical line, so loading twice finds the record
  again. An existing record is left alone when it already holds the line's values (numbers compare by value,
  dates by instant prefix, write-only fields such as passwords are not compared) and patched when it does
  not. The report counts `created`, `updated`, `unchanged` and `failed` per collection.
- **Order.** Files are loaded in collection-name order; a record that fails is retried after the others (up
  to four passes), so a relation to a collection seeded later resolves.
- **Errors** are `file:line: collection record id: code: message (field: reason)`; the lines that are
  valid still load. A plain `dev --seed` exits 1 when a record fails.

`dev reset` on a running emulator deletes every record of the project's collections (views excepted) and
seeds again. With no emulator running, `dev reset --persist DIR` empties `DIR`, after checking it looks like
a dev directory (`dev-credentials.json` or `data/`).

### 4. `--watch` polls

A watcher scans the project directory every 250 ms (path, mtime, size; `.git`, `node_modules`, `.schemas`,
`target` and `dist` skipped) and reports a change once the directory has been quiet for 400 ms, so one save
of several files, or a formatter or build writing after it, is one re-apply. It polls instead of using
`notify`: no dependency (and so no licence to record), the same behaviour on every filesystem and in
containers, and a deterministic test. Files an apply wrote itself (a function build) are taken as the new
baseline. Each change prints the plan, the outcome, and with `--json` an event line.

### 5. Machine-readable readiness

`--json` prints exactly one line on standard output when everything (apply, seed) is done:

```json
{"event":"ready","url":"http://127.0.0.1:53211","port":53211,"admin":{"email":"...","password":"..."},
 "api_key":"fbk_...","database":"sqlite","data_dir":"...","persist":false,"project":"forgebase",
 "applied":3,"seed":{"collections":{"todos":{"created":3,"updated":0,"unchanged":0,"failed":0}},"errors":[]},
 "pid":4242}
```

Progress and logs go to standard error. With `--watch` further lines have `"event":"reapplied"`. A test
suite starts `forgebase dev --port 0 --json`, reads until `event == "ready"`, and stops it with SIGINT; the
recipes are in [docs/dev-workflow.md](../dev-workflow.md#4-test-suites).

### 6. Finding a running emulator

`dev seed` and `dev reset` find the emulator that serves the project directory through a registry file in
the temp directory (`forgebase-dev/<digest of the canonical project path>.json`, mode 0600: URL, pid, the
dev credentials) that the emulator writes when ready and removes on exit. A stale file is harmless: the
command checks `/health` first and says so.

### 7. `forgebase gen types`

`gen types --lang ts|rust [--from-project DIR | --url SERVER] [--out FILE] [--check]` prints typed models of
every collection.

- **One model, two sources.** Both a project directory and a server are turned into ADR-0037 §4 canonical
  collection specs and from them into one language-neutral model (`gen/model.rs`); emitters print it. A
  project that does not declare `users` still gets the built-in auth collection, so `--from-project` and
  `--url` agree (tested).
- **Determinism.** Collections sorted by name, fields in declared order, no timestamp or path in the
  output, `\n` line endings, a `format` number in the header. Nothing is rewritten when the file is
  already identical.
- **`--check`** writes nothing and exits **2** when `--out`'s file is missing or differs (the exit code of
  `plan` and `drift` for "differs"; 1 stays for errors), so CI can require generated types to be committed.
- **TypeScript** uses object `type` aliases, not interfaces: an interface is not assignable to the SDK's
  `Row = Record<string, unknown>` bound, a type alias is. `fb.collection<Todos>("todos")` type-checks
  against `@forgebase/sdk` (the test compiles the output with `tsc` against `sdk/js/src`). Per collection:
  `Todos` (what the API returns: system fields `id`, `created`, `updated`, `_version`; a field that is not
  required is `T | null`; a hidden field is optional; passwords are never returned), `TodosCreate` (required
  fields without a default are required, the rest optional; `id?` lets the caller pick the id),
  `TodosUpdate = Partial<TodosCreate>`, a union alias per `select` field, and the maps `Collections`,
  `CollectionInputs`, `CollectionName` and `collectionNames`.
- **Rust** emits `serde` structs: `Todos` (+ `Todos::COLLECTION`), `TodosCreate` (+ `new(required fields)`), `TodosUpdate` (all
  `Option`, `Default`), an enum per `select` field, `GeoPoint` and `COLLECTION_NAMES`, usable as
  `fb.collection(Todos::COLLECTION).typed::<Todos>()` with the Rust SDK. Names that differ from Rust's conventions
  get `#[serde(rename)]`; keywords become raw identifiers. The file is a module file (`mod types;`).
- **Field types**: text/email/url `string`; date RFC 3339 `string`; number `number` / `i64` / `f64`; bool;
  select literal union / enum (a multi-select is an array); json `JsonValue` / `serde_json::Value`; array
  of text, number, bool or json; relation and file `string` or `string[]`; geo `{lat, lon}`; vector
  `number[]` / `Vec<f32>`. A flexible-schema collection accepts further fields (`& Record<string,
  unknown>` / a flattened `extra` map). A view is a plain map. An auth collection has `email`,
  `email_visibility`, `verified` and a write-only `password`.
- Dart, Kotlin and Swift are not generated yet (follow-up): the model is language-neutral, an emitter is
  one file.

### 8. Function debugging

`forgebase functions invoke <name> [--data JSON|@file] [--method M] [--path /sub?query] [--trace] [--json]`
calls the function over HTTP with a request id of its own (`X-Request-Id: fbcli-...`) and shows the status,
the round-trip time, the request id, the **trace id** (the execution log's `trace_id`, else the response's
`traceparent`), the response, and the execution log row found by that request id (version, trigger, status,
server-side duration, fuel, error, **log lines**). Exit code 1 when the HTTP status is not 2xx.

`--trace` asks the insights API of ADR-0036 (`GET /api/v1/insights/functions/invocations?request_id=...`)
for the execution's profile and prints cold start, fuel and memory against their limits, the host-call
timeline (operation, offset, duration, ok) and the per-operation totals. A server without that route (404),
a credential that may not read it (401, 403) or an execution without a profile (profiling off) gets one
explaining line instead; the logs and the trace id are always shown, so the command works the same against
servers with and without ops insights.

`functions logs <name> --follow` already polled for new executions (ADR-0017); it now prints the trace id of
each execution, and a poll that fails (a server that restarts, an emulator that reloads) is reported once and
retried instead of ending the stream.

The probe client (`probe.rs`) gained request headers and the response headers (additively), and the CLI's
scripted HTTP test server (`testserver.rs`) a handler form with response headers.

## Consequences

- `forgebase dev` needs no Docker and no second binary, and starts in about a second on SQLite.
- The emulator is as faithful as the server because it is the server. The cost: the CLI links the server (it
  already did, for `serve`).
- The polling watcher costs a directory scan per 250 ms; project directories are small. A project with
  thousands of seed rows is not rescanned for content, only for mtime and size.
- Generated TypeScript uses `null` for unset fields because the API does; consumers that prefer `undefined`
  can wrap it.
- Generated types describe the schema at generation time; `--check` in CI keeps them honest.

## Limitations and follow-ups

- Dart, Kotlin and Swift emitters.
- `ctx.trace` and spans of a function's host calls in the OTLP export (ADR-0035 follow-up) would let `--trace`
  link to a collector's view.
- `dev --watch` rebuilds every function with a `build` command on each change; a per-function dependency
  check is future work.
- Seed files cannot reference records by something other than their id (no `$ref` syntax); fixtures that
  relate use fixed ids.
- `dev reset` deletes record by record; a bulk delete endpoint would make large fixtures faster.
- Webhook push deliveries from a dev emulator go to the URLs in the project; `forgebase queues listen
  --forward-to` is the companion for local receivers.
