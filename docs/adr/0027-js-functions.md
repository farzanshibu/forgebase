# ADR-0027: JavaScript and TypeScript functions on QuickJS in WebAssembly

- Status: accepted
- Date: 2026-11-11
- Work package: FB-93 (contract: `docs/requirements/edge-functions.md` §1-3)
- Builds on: ADR-0017 (functions on Wasmtime), ADR-0008 (policy engine),
  ADR-0011 (outbox), ADR-0018 (object storage), ADR-0022 (queues),
  ADR-0023 (jobs).

## Context

Functions so far are WebAssembly modules written against a raw ABI. The
human asked for Supabase-style edge functions written in TypeScript or
JavaScript in the dashboard, using the database, vector search, key-value
data, jobs, queues, storage and auth with row-level security everywhere, and
decided the runtime: **QuickJS compiled to WebAssembly inside the existing
Wasmtime sandbox**. No new trust boundary: JavaScript runs inside the same
sandbox, with the same limits, as any WebAssembly function.

## 1. The engine module

- **QuickJS**: quickjs-ng **0.17.0** (MIT): `quickjs.c`, `libregexp.c`,
  `libunicode.c`, `dtoa.c`; not `quickjs-libc` (no filesystem, no OS module).
- **Glue**: `crates/forgebase-functions/js/engine.c` (natives: host call,
  log, UTF-8, base64, CSPRNG, monotonic clock; the module loader that knows
  only `"forgebase"`; the job/timer loop) and `js/prelude.js` (Web globals,
  `serve`, `ctx`), embedded into the C file with C23 `#embed`. The prelude's
  source is `js/prelude.ts` (typed, `tsconfig.json` beside it); `prelude.js`
  is generated from it by `scripts/build-js-prelude.ts`, which only blanks out
  the types, so line and column numbers in error traces match the `.ts` file.
- **Build**: `scripts/build-js-engine.sh` with **wasi-sdk 34.0** (clang 23)
  for `wasm32-wasip1`, reactor model, `-O2 -g0`, 1 MiB stack,
  `--strip-all`, `-ffile-prefix-map` for stable paths; tool and source
  tarballs pinned by SHA-256 (the wasi-sdk digests are GitHub's). The output
  `js/forgebase-js.wasm` (about 1.06 MiB) is committed with
  `js/forgebase-js.wasm.sha256` and `js/LICENSES.md` (QuickJS MIT; wasi-libc
  MIT option; musl MIT; dlmalloc CC0; compiler-rt Apache-2.0 WITH
  LLVM-exception). It is embedded with `include_bytes!`; users never need a
  toolchain. A unit test checks the embedded bytes against the checksum file;
  the CI job `js-engine` rebuilds with `--check` and fails on any difference.
- **Imports**: the `forgebase` I/O functions (`input_len/read`,
  `source_len/read`, `output`, `result_read`, `log`), one generic
  `call(op, arg)`, and six WASI functions answered by hand-written shims
  (`clock_time_get`, `random_get`, `fd_write` into the execution log;
  `fd_close`, `fd_fdstat_get`, `fd_seek` return `EBADF`). No WASI
  filesystem, environment, sockets or `proc_exit`. A unit test pins the
  import list.

## 2. Execution

- A separate Wasmtime `Engine` (same configuration as ADR-0017 plus a 1 MiB
  native stack for the interpreter; its own epoch ticker). The module is
  compiled once per process on first use.
- **Snapshot**: starting QuickJS and evaluating the prelude costs about
  40 M fuel. Once per process the host instantiates the engine in a store of
  its own, calls `_initialize` and `init`, and copies its linear memory and
  `__stack_pointer` (its only mutable global, exported for this). Every
  execution instantiates a **fresh instance**, writes the snapshot into its
  memory and sets the stack pointer, then calls `handle`. No function code
  has run in the snapshot, so nothing leaks between executions; per-execution
  state is set in `begin()`: `Math.random` is replaced by xoshiro128** seeded
  from the host CSPRNG, `performance.now()` gets a new origin. A minimal
  function costs about 1.7 M fuel instead of 41.8 M.
- `handle` evaluates the version's JavaScript as an ES module named
  `index.js` (top-level `await` allowed), then calls the handler registered
  with `serve()` and drives QuickJS's job queue and the prelude's timers
  until the response promise settles. A timer wait is a host `sleep`, bounded
  by the execution's deadline.
- **Limits are ADR-0017's**: fuel, memory (the snapshot counts; a JS version
  needs `limits.memory_bytes` ≥ 4 MiB, else `memory_limit_too_small` at
  deploy), wall clock (epoch deadline plus the Tokio timeout, which also ends
  sleeps), concurrency, nesting, 10 000 host calls, 16 fetches. Responses
  ≤ 4 MiB (decoded body), requests ≤ 1 MiB. QuickJS's own stack check (768
  KiB) turns deep recursion into a `RangeError`.
- **Errors**: an uncaught exception is `function_failed` with its trace;
  positions are rewritten to the user's source (TypeScript through the stored
  source map, see §3) and the trace is also an `error` line in the execution
  log. Limit errors say which limit and how to raise it.

## 3. Deploy and TypeScript

- `POST /api/v1/functions` takes `source` + `language` (`typescript`
  default, `javascript`) instead of `wasm`. Versions record
  `runtime` (`js`|`wasm`) and `language` (`typescript`|`javascript`|`wasm`)
  in their configuration JSON; rows written before migration 0027 lack them
  and read as `wasm`.
- **oxc 0.155.0** (MIT): parse as an ES2023 module (TypeScript syntax for
  `typescript`), semantic checks with syntax errors and enum evaluation,
  oxc's TypeScript transform (types, interfaces, enums, namespaces,
  parameter properties) and its code generator with a source map. JavaScript
  is stored as written. Errors are `400 function_compile_failed` with
  `details = {line, column, message}` of the first and `details.errors` with
  all (at most 20), 1-based, in characters.
- A function is **one file**: imports other than `"forgebase"`, re-exports
  from other modules, `import()` and `import = require()` are compile
  errors (relative, `npm:` and URL imports are not supported in v1).
- **Static capability check**: the same pass collects services the code
  visibly uses (`ctx.db.from(..).create(..)`, `ctx.kv.get(..)`,
  `ctx.admin`, `fetch(..)`, `ctx.env.get`, ...). A use whose capabilities
  are all missing is `400 capability_not_granted` with the position. The scan
  does not see computed names or destructuring; the check at call time is
  authoritative.
- Stored per version (migration **0026**): `source` (as deployed), `js`
  (what runs), `source_map` (TypeScript: sorted
  `[gen line, gen col, src line, src col]` entries), `wasm` empty.
  `GET /functions/{name}/versions/{v}/source` returns `{language, source}`
  (`404 function_source_not_found` for WebAssembly).
- `GET /api/v1/functions/types` returns `sdk/functions/forgebase.d.ts` as
  `application/typescript` (JSDoc with an example on every member).
- `POST|ANY /functions/{name}/invoke[/{*path}]`: a JavaScript function's
  `req.url` is `<proto>://<host>/<path>?<query>` (header
  `x-forwarded-prefix` carries `/api/v1/functions/<name>/invoke`); the raw
  body is passed through unchanged. `?version=<n>` from an administrator runs
  that version (trigger `test`, the parameter is removed from the query); for
  other callers it is an ordinary query parameter. The response's
  `x-request-id` is the execution log's `request_id`.
- CLI: `forgebase functions new <name> [--js] [--dir functions]` writes
  `index.ts`/`index.js` from a template plus `forgebase.d.ts`;
  `forgebase functions deploy <name> <file>` sends `.ts`/`.mts` as TypeScript,
  `.js`/`.mjs` as JavaScript, anything else as a WebAssembly module.

## 4. The function API and its host operations

`serve(handler)` (from `"forgebase"`, also global and `Deno.serve`) registers
`(req: Request, ctx) => Response | JSON | Promise<…>`. Web globals: `Request`,
`Response` (`json`, `redirect`, `error`), `Headers`, `URL`,
`URLSearchParams`, `TextEncoder`, `TextDecoder`, `Blob`, `atob`, `btoa`,
`crypto.randomUUID`, `crypto.getRandomValues`, `crypto.subtle.digest`
(SHA-1/256/384/512) and HMAC `importKey`/`sign`/`verify`, `structuredClone`,
`setTimeout`/`setInterval`, `queueMicrotask`, `AbortController`, `fetch`
(the SSRF-safe fetcher and `fetch_hosts` of ADR-0017 §8), `console.*` (execution
log lines; always available to JavaScript), `Deno.env.get`/`ctx.env.get`
(plain env, then secrets). No streams: bodies are bytes in memory.

Every `ctx` service runs **as the caller** (`HostEnv::caller`): HTTP — the
invoker; before-hooks — the writer; after-hooks and jobs — the function's own
identity `service:function.<name>` (ADR-0017 §4). So collection rules, field
rules, record ACLs, tenancy, bucket ACLs and policies apply unchanged:

| `ctx` | Host op(s) | Capability | Authorised by |
|---|---|---|---|
| `db.from(c).get/list` | `db.get`, `db.list` | `db.read` | data core + policy engine (rules, field rules, ACLs, tenancy) |
| `db.from(c).create/update/delete` | `db.create/update/delete` | `db.write` | same |
| `vector.from(c).search/get` | `vector.search` (`text` → hybrid), `vector.get` | `vector.read` | `engine:vector_read` |
| `vector.from(c).upsert/delete` | `vector.upsert`, `vector.delete` | `vector.write` | `engine:vector_write` |
| `kv.*` (private `fn.<name>`) | `kv` | `kv` | the capability (the function's own data, in the caller's tenant) |
| `kv.namespace(ns).*` | `kv` | `kv` | a namespace rule **and** the caller's `engine:kv_read/write` |
| `storage.from(b).get/list/signedUrl(GET)` | `storage.get/list/sign` | `storage.read` | bucket owner, visibility, ACL |
| `storage.from(b).put/delete/signedUrl(PUT)` | `storage.put/delete/sign` | `storage.write` | same (≤ 4 MiB per call) |
| `queues.publish` | `queues.publish` | `queues.publish` | `engine:queue_publish` |
| `jobs.enqueue` | `jobs.enqueue` | `jobs.enqueue` | `engine:enqueue_job` |
| `realtime.broadcast` | `realtime.broadcast` | `realtime.publish` | channel publish policy |
| `auth`, `verifyToken`, `auth.verify` | `auth.verify` | none | access tokens, API keys, PATs |
| `env.get` | `env.get` | `env` or `secrets` | — |

`ctx.auth` is `{id, type, collection, roles, permissions, realm, org,
tenant}` (permissions: the policy engine's granted patterns) or `null` for
anonymous callers. Host ops answer `{code, message}` on failure; the prelude
throws `ForgeBaseError` with that `code`. A missing capability says which
API needs which capability and how to add it.

Queues reach the functions crate through the `QueuePublisher` trait
(`forgebase-queue` depends on this crate); the server attaches every engine
with `FunctionService::attach_services` (`forgebase-server/src/function_services.rs`).

**Key-value namespaces.** KV had no namespace rules: its authorizer checks
actions only. Functions get admin-managed, deny-by-default rules per
function: `GET|PUT /api/v1/functions/{name}/kv-rules`
`{"items":[{"namespace":"app"|"app.*"|"*","access":"read"|"write"}]}`
(table `_forgebase_function_kv_rules`; `PUT` needs an administrator). A
`read` rule allows only read commands. The caller's KV permissions apply on
top. The KV studio and RESP are unchanged.

## 5. Service role

The capability `service_role` (JavaScript only) makes `ctx.admin.*` — the
same services — run as the function's **privileged** principal
`service:function.<name>` (admin service principal, in the caller's realm,
organization and tenant), bypassing rules, ACLs and namespace rules: the
Supabase `service_role` equivalent. Only an administrator (`is_privileged`)
can deploy a version that requests it (`403 service_role_requires_admin`
otherwise; a manager with `manage_functions` can still deploy versions
without it). Every such deploy writes `functions.service_role_granted` to
`_forgebase_auth_audit` in the deploy transaction (actor, request id,
function, version, SHA-256, capabilities) and a warning to the server log.
Without the capability every `ctx.admin` call fails with
`capability_denied` (and visible uses fail the deploy).

## Consequences and limitations

- Executions start in well under a millisecond of host work beyond the
  snapshot copy (about 2 MiB); the first JavaScript execution of a process
  compiles the engine and takes the snapshot.
- QuickJS is an interpreter: CPU-heavy code is slower than in V8; fuel
  limits make that visible rather than unbounded.
- No streaming bodies, `WebSocket`, `FormData` uploads, Node or npm
  modules, or multi-file functions in v1.
- The static capability scan is best effort; columns in traces are QuickJS's
  (characters of the generated code mapped to the nearest source token).
- Function-to-function cycles through after-hooks are not detected beyond
  ADR-0017's self-loop rule; JavaScript after-hooks run as the function, so
  their own writes do not re-trigger them.
