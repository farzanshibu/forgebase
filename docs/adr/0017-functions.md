# ADR-0017: Server functions on Wasmtime

- Status: accepted
- Date: 2026-11-09
- Work package: FB-60 (MDF-33)
- Scope: WebAssembly runtime, capability-based host API, resource limits,
  versions and rollback, record hooks, HTTP functions, scheduled/background
  functions, sealed secrets, SSRF-safe outbound HTTP, execution logs.
- Builds on: ADR-0003 §7 (`AccessContext`, `Authorizer`), ADR-0007 (jobs),
  ADR-0008 (policy engine), ADR-0011 (transactional outbox), ADR-0015 §2
  (outbound TLS client).

## Context

SPEC §13 asks for sandboxed functions (Supabase Edge Functions-like) on
Wasmtime: HTTP functions, before/after create/update/delete hooks,
scheduled and background functions, versioning, environment and secrets,
logs and metrics, timeouts, memory, fuel and concurrency limits,
permissions and rollbacks, with capability-based security and no
unrestricted filesystem, network or process access.

## 1. Crate and wiring

New crate `forgebase-functions` (depends on data, db, api, auth, jobs, kv,
realtime). `forgebase-server` opens it after realtime and KV, mounts its
`ApiExtension`, registers its job handler and starts its after-hook
dispatcher. System migration **0016** adds the tables.

## 2. Runtime

- **Wasmtime 50.0.1** (`default-features = false`; `runtime`, `cranelift`,
  `async`, `std`): no WASI, no component model, no GC, no threads, no
  compilation cache on disk. Modules are core WebAssembly in binary form,
  compiled once on a blocking thread at deploy and cached in memory per
  `(function, version)`.
- **One fresh `Store` and instance per execution.** Nothing survives between
  executions except what the function writes through its capabilities.
- **Fuel** (`consume_fuel`): every execution gets `limits.fuel`; host calls
  also cost fuel (10 000 plus one per argument byte). Exhaustion traps:
  `function_fuel_exhausted`.
- **Memory**: a `ResourceLimiter` refuses growth of linear memory beyond
  `limits.memory_bytes` and traps (`function_memory_limit`); one memory, one
  instance, at most 4 tables. A module whose *initial* memory exceeds the
  limit is refused at deploy (`memory_limit_too_small`).
- **Wall clock**: epoch interruption (a ticker thread advances the engine
  epoch every 5 ms; the thread holds only a weak engine reference) traps a
  busy guest; a Tokio timeout around the whole execution also bounds time
  waiting in host calls such as fetch (`function_timeout`, HTTP `504`).
- **Cooperative scheduling**: the guest yields to Tokio every 1 000 000 fuel
  units (`fuel_async_yield_interval`), so long computations never block a
  runtime thread for long.
- **Concurrency**: a semaphore per function (`limits.max_concurrency`) and
  one for the engine (`FunctionsConfig::max_concurrent`, 64); over the limit
  is `function_busy` (HTTP `429`), never a queue.
- **Nesting**: executions nest (a function's write runs another function's
  before-hook) at most 4 levels (`function_depth_exceeded`), tracked with a
  Tokio task-local.

Defaults: 100 000 000 fuel, 16 MiB, 5 s, 8 concurrent. Ceilings:
20 000 000 000 fuel, 512 MiB, 60 s, 256.

## 3. Host API (ABI)

Imports come only from module `forgebase`; the module must export
`memory` (32-bit, unshared) and `handle: () -> i32` (0 = success).

| Import | Capability | Purpose |
|---|---|---|
| `input_len() -> i32`, `input_read(ptr, len) -> i32` | — | the execution's input (JSON) |
| `output(ptr, len) -> i32` | — | the execution's output (≤ 4 MiB) |
| `result_read(ptr, len) -> i32` | — | copy out the last host call's result |
| `log(level, ptr, len) -> i32` | `log` | 0 debug, 1 info, 2 warn, 3 error (200 lines, 64 KiB kept) |
| `db_get`, `db_list` | `db.read` | `{"collection","id"}`, `{"collection","filter","params","sort","page","per_page","cursor"}` |
| `db_create`, `db_update`, `db_delete` | `db.write` | `{"collection","data"}`, `{"collection","id","data","expected_version"}`, `{"collection","id"}` |
| `kv` | `kv` | `{"op":"get"|"set"|"delete"|"incr","key","value","ttl_ms","by"}` in namespace `fn.<name>` |
| `env_get` | `env` | raw key → raw value of the version's plain `env` |
| `secret_get` | `secrets` | raw name → raw value of a sealed secret |
| `fetch` | `fetch` | `{"url","method","headers","body"|"body_base64"}` → `{"status","headers","body"|"body_base64","url"}` |

Capability calls take `(ptr, len)` of a JSON (or raw) argument and return
`n ≥ 0` (success, `n`-byte result) or `-n` (an `n`-byte
`{"code","message"}` error); at most 10 000 host calls per execution.

**Capabilities are enforced twice**: at deploy, a module importing a host
function whose capability its version is not granted is refused
(`capability_not_granted`), as is any import outside the table or from
another module (`invalid_module`); at run time each call checks again.

## 4. Identity and authorisation

A function acts as **`service:function.<name>`**, a non-admin service
principal (`forgebase_functions::identity`). Its database and KV calls go
through the data core and therefore through the installed `Authorizer` (the
policy engine): it can do exactly what an administrator granted that
principal with the policy API (overrides, realm bindings, roles), e.g.
`data:notes:write`, `engine:kv_write`. The caller's identity is passed to
the function as data (`auth: {type, id}`) but never lent to it.

New data-core actions (policy keys through `engine:<action>`):
`read_functions`, `manage_functions` (deploy, rollback, delete, secrets) and
`invoke_function` (HTTP functions with access mode `permission`).

## 5. Versions and rollback

Every deploy inserts an immutable row in `_forgebase_function_versions`:
the module, its SHA-256 and its configuration (capabilities, `fetch_hosts`,
limits, `env`, hooks) as one JSON document. `_forgebase_functions.active_version`
names the version that runs; `activate: false` deploys without switching.
`POST /functions/{name}/rollback` activates `{"version": n}` or, without a
body, the newest version before the active one — module and configuration
together. Versions are never deleted except with the function.

## 6. Triggers

- **HTTP**: `ANY /api/v1/functions/{name}/invoke` (≤ 1 MiB body). Access
  modes: `public` (anonymous allowed), `authenticated`, `permission`
  (default). Input `{trigger, method, path, query, headers, body |
  body_base64, auth, request_id}`; `authorization`, `cookie`, `x-api-key`,
  `x-csrf-token` are never passed in. Output `{status, headers, body}` (or
  empty: `204`); `Set-Cookie`, hop-by-hop and length headers from the
  function are dropped; invalid output is `502 function_bad_output`; traps,
  fuel and memory `502`, timeouts `504`.
- **Before-hooks** (`before_create|update|delete`): a `WriteGuard` installed
  on the data service runs after the caller's write was authorised and
  **before its transaction starts**, so hooks never hold the single writer.
  Hooks of the collection and event run in name order; output
  `{"allow": false, "reason"}` rejects (`400 hook_rejected`). A hook that
  fails (trap, fuel, memory, timeout, busy, bad output) rejects too
  (`hook_failed`): fail closed. System-principal writes skip hooks.
- **After-hooks** (`after_create|update|delete`): a dispatcher tails the
  realtime outbox (ADR-0011) from a cursor in `_forgebase_function_cursors`
  (initialised at the outbox head on first start), so hooks run only for
  committed changes, at least once, in commit order, including changes made
  while no dispatcher ran. Failures are logged, not retried. A function's own
  writes do not trigger its own after-hooks.
- **Jobs**: job kind `function.invoke`, payload `{"function", "input"}`;
  schedules of that kind are scheduled functions. Timeouts and `busy` are
  retried with the queue's backoff; faults dead-letter.

## 7. Secrets

`PUT /functions/{name}/secrets/{key}` seals the value with XChaCha20-Poly1305
under the auth keyring (key derived from `<data>/auth/master.key`), random
24-byte nonce, AAD `forgebase/functions/secret/v1\0<function id>\0<name>`
(a ciphertext cannot be moved to another function or name). Values are
never returned by the API and are decrypted only for executions of versions
granted `secrets`. Plain `env` values are part of the version (not secret).

## 8. SSRF-safe fetch

Per hop (the first URL and every redirect target):

1. `http`/`https` only, a host, no credentials in the URL, host on the
   version's `fetch_hosts` if any (`example.com` or `*.example.com`).
2. Resolve once; refuse if **any** address is in a blocked range: IPv4
   0/8, 10/8, 100.64/10, 127/8, 169.254/16 (incl. 169.254.169.254), 172.16/12,
   192.0.0/24, documentation ranges, 192.88.99/24, 192.168/16, 198.18/15,
   224/4, 240/4; IPv6 `::`, `::1`, fc00::/7, fe80::/10, fec0::/10, ff00::/8,
   100::/64, 2001::/23 (incl. Teredo), 2001:db8::/32, 3fff::/20, and IPv4
   embedded in IPv6 (mapped, compatible, NAT64 64:ff9b::/96 and /48, 6to4).
3. Connect to a vetted address; the name is not resolved again, so a DNS
   answer that changes between check and connect (rebinding) cannot reach an
   internal address. TLS (FB-25 rustls settings) verifies the certificate for
   the host name.

Redirects (301/302/303/307/308) are followed by the fetcher, at most 5;
`Authorization`, `Cookie` and `Proxy-Authorization` are dropped when the
origin changes. Limits: 10 s per request, 2 MiB response, 1 MiB request,
16 requests per execution. `Resolver` and `Connector` are traits so tests
(and egress proxies) can substitute them.

## 9. Execution logs and metrics

One row per execution in `_forgebase_function_logs` (trigger, status,
duration, fuel used, error, logged lines, request id), newest 1 000 kept per
function. Metrics: `forgebase_function_executions_total{function,status}`,
`forgebase_function_duration_seconds{function}`,
`forgebase_function_deploys_total`.

## 10. Changes outside the crate

- `forgebase-data`: `WriteGuard`/`PendingWrite`/`GuardRejection` and
  `DataService::set_write_guard` (called from create/update/delete before
  the transaction); actions `ReadFunctions`, `ManageFunctions`,
  `InvokeFunction`.
- `forgebase-realtime`: `outbox::read_after` and `outbox::bounds` are public.
- `forgebase-auth`: `net::http1::exchange` is public (fetch connects to the
  vetted address itself, then uses FB-25's HTTP/1.1 exchange).
- `forgebase-server`: wiring; `RunningServer::functions()`.

## Consequences and limitations

- Guests are written against a raw ABI (no SDK yet; FB-62 or later can add
  Rust/AssemblyScript helpers). No WASI means no clock or randomness inside
  functions.
- Before-hooks can reject but not modify the written data.
- After-hooks are at-least-once and not retried; cycles between two
  functions' after-hooks are not detected (self-loops are).
- Functions are instance-wide (not per tenant); their identity has no
  tenant unless granted through the policy engine.
- Modules are stored in SQLite (≤ 16 MiB each) and recompiled after a
  restart on first use.
- The functions dashboard and `forgebase functions deploy/logs` are FB-62.
