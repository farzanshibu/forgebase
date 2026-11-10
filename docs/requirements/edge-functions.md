# Edge functions in TypeScript and JavaScript (FB-93, FB-94)

Human request (2026-11-11): Supabase-style edge functions, written in the
dashboard with an online code editor, that can use the database, vector
search, the Redis-style key-value store, background jobs and queues, storage and
auth, with row-level security applied everywhere. Runtime decision (human):
**QuickJS compiled to WebAssembly**, running inside the existing Wasmtime
sandbox (ADR-0017). Both packages build against this document; change it only
through god.

## 1. Runtime (FB-93)

- A JavaScript engine module (QuickJS built for wasm32-wasip1, or a Rust
  wrapper around it) is embedded in the binary with `include_bytes!`. It is
  built by a committed, reproducible script (`scripts/build-js-engine.sh`, with
  pinned tool versions) and committed as an artifact with its SHA-256 and
  licence notices (QuickJS is MIT). Users never need a toolchain. CI rebuilds it
  and fails if the checksum differs, or at least checks that it builds.
- A JS/TS function runs through the existing executor with everything
  ADR-0017 already gives a WebAssembly function: one fresh instance per
  execution, fuel, memory limit, wall-clock timeout, concurrency limit,
  capabilities checked at deploy and at call time, versions and rollback,
  sealed secrets, plain env, SSRF-safe fetch, execution logs and metrics, and
  every trigger (HTTP invoke, before- and after-hooks, jobs).
- **TypeScript** is type-stripped to JavaScript on the server at deploy time,
  with a pure-Rust transformer (oxc or swc; MIT, Apache or BSD licensed). The
  original source, the language and the stripped JavaScript are stored with the
  version. Syntax and transform errors return `400 function_compile_failed` with
  line and column.
- Modules: ES2023 with a single entry file. Relative imports between files of
  the same function and `npm:` or URL imports are **not** supported in v1;
  return a clear compile error. The editor uses one file per function.

## 2. Function API inside the code (FB-93)

```ts
import { serve } from "forgebase";

serve(async (req: Request, ctx) => {
  // ctx.auth: { id, type, roles, permissions, realm } or null for anonymous
  const todos = await ctx.db.from("todos").list({ filter: "done = false" });
  return Response.json(todos);
});
```

- Web-standard globals: `Request`, `Response` (including `Response.json`),
  `Headers`, `URL`, `URLSearchParams`, `TextEncoder`, `TextDecoder`, `atob`,
  `btoa`, `crypto.randomUUID`, `crypto.getRandomValues`, `crypto.subtle.digest`
  (SHA-256 at least), `structuredClone`, `setTimeout` (within the time limit),
  `fetch` (the existing SSRF-safe host fetch and `fetch_hosts` list), and
  `console.log/info/warn/error` mapped to execution logs. `Deno.env.get(name)`
  and `ctx.env.get(name)` read plain env first, then secrets.
- `ctx` services. Every call runs **as the caller** by default, so collection
  rules, field rules, record ACLs, tenancy and policies apply (ForgeBase's
  row-level security):

| Service | API sketch | Capability |
|---|---|---|
| Records | `ctx.db.from(c).get(id)`, `.list({filter, sort, page, perPage, cursor})`, `.create(data)`, `.update(id, data, {expectedVersion})`, `.delete(id)` | `db.read`, `db.write` (existing) |
| Vector | `ctx.vector.from(c).search({vector or text, k, filter})`, `.upsert`, `.delete` | `vector.read`, `vector.write` |
| Key-value (Redis-style) | `ctx.kv.get/set(key, value, {ttlMs})/del/incr/expire/exists/keys(prefix)`, hashes `hget/hset/hdel/hgetall` | `kv` (namespace rules below) |
| Storage | `ctx.storage.from(bucket).get(key)`, `.put(key, bytes, {contentType})`, `.list(prefix)`, `.delete(key)`, `.signedUrl(key, {expiresIn})` | `storage.read`, `storage.write` |
| Queues | `ctx.queues.publish(topic, message, {key})` | `queues.publish` |
| Jobs (background) | `ctx.jobs.enqueue(kind, payload, {runAt, maxAttempts})`; enqueueing `function.invoke` runs another function in the background | `jobs.enqueue` |
| Realtime | `ctx.realtime.broadcast(channel, event, payload)` | `realtime.publish` |
| Auth | `ctx.auth` (caller); `ctx.auth.verify(token)` for a bearer token in a webhook | none |

- Key-value namespaces: the existing function-private namespace `fn.<name>`
  stays the default. Access to application namespaces goes through the same
  policy engine as the KV studio. If KV has no rules today, add namespace-level
  allow rules, admin-managed, with deny by default.
- **Service role.** A version may request the `service_role` capability, which
  only an administrator can grant at deploy time. The code then calls
  `ctx.admin.db` / `ctx.admin.vector` / … with the same API, bypassing row
  rules: the Supabase `service_role` equivalent. It is logged in the audit log
  on every deploy that grants it.
- Limits stay those of ADR-0017. The response body is at most 4 MiB; the
  request body for HTTP invoke is at most 1 MiB.

## 3. Deploy API and CLI (FB-93)

- `POST /api/v1/functions` (existing deploy, name in the body) also accepts
  `{"name", "source": "<code>", "language": "typescript" | "javascript", "capabilities": [...], "env": {...}, "fetch_hosts": [...], "limits": {...}}`
  instead of `wasm`. Compile errors: `400 function_compile_failed` with
  `details: {"line","column","message"}` (first error, 1-based) and
  `details.errors: [{line,column,message}]`. A non-administrator requesting
  `service_role` gets `403`. The response and the versions list carry `language` and
  `runtime` (`"js"` or `"wasm"`).
- `GET /api/v1/functions/{name}/versions/{version}/source` returns
  `{"language","source"}` for JS/TS versions (`404` for WebAssembly).
- `GET /api/v1/functions/types` returns the `forgebase` module `.d.ts` (the
  function API above) for the editor. The same file is committed at
  `sdk/functions/forgebase.d.ts`.
- `/api/v1/functions/{name}/invoke` (existing, any method) is unchanged; add
  `/invoke/{*path}` (the sub-path and query reach `req.url`) and
  `?version=<n>` for a test run against a specific version (administrator
  only). The response carries `x-request-id`, matching the execution's entry in
  `GET …/logs`.
- CLI: `forgebase functions new <name> [--js]` writes `functions/<name>/index.ts`
  plus `forgebase.d.ts` from a template. `forgebase functions deploy <name> <file>`
  accepts `.ts`, `.js` or `.wasm`.
- ADR-0027 records the engine, its build, the TypeScript transform, the host
  bindings, service role and the limits.

## 4. Dashboard (FB-94)

In the Functions studio, on PanelUI:

- **New function.** Name, a template (HTTP API, webhook with signature check,
  database after-hook, background job, scheduled job via jobs), language
  (TypeScript default), and capabilities as switches. Opens the editor.
- **Editor.** Monaco, lazy-loaded with its workers, bundled offline (no CDN),
  so the first page load stays as it is. TypeScript IntelliSense uses the
  `forgebase` types from `GET /api/v1/functions/types`. The theme follows
  light/dark mode. `Ctrl/Cmd+S` saves a draft in the browser; **Deploy** sends
  `POST /api/v1/functions` and shows compile errors inline on their line.
- **Test panel.** Method, path, query, headers and JSON body →
  `POST …/invoke?version=` → status, headers, body and the execution's logs.
- **Versions.** Lists runtime and language. A JS/TS version opens read-only in
  the editor, with a diff against the current version and the existing
  rollback.
- Keep the WebAssembly upload as an alternative in New function.
- If FB-91 marks `functions` disabled, the studio shows the feature-disabled
  panel (FB-92 provides it).

## Shared rules

- Next free numbers after FB-90..92: migration `0027` if FB-91 or FB-90 takes
  `0026` (check main before choosing; god renumbers on merge if needed), ADR
  `0027`.
- Do not put API keys, passwords or real data in templates, examples or tests.
