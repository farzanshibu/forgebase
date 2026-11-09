# ForgeBase SDKs

This page says which SDKs exist, what they cover, and where their full
documentation is. The JavaScript SDK's own README in
[`sdk/js/README.md`](../sdk/js/README.md) is the detailed reference. This page
does not repeat it.

## 1. Status

| SDK | Location | Status |
|---|---|---|
| JavaScript / TypeScript (`@forgebase/sdk`) | [`sdk/js`](../sdk/js) | Version 0.1.0. Covers auth, collections and records, realtime, and entitlements. |
| Rust | Not in the repository yet | In progress as work package FB-82, which depends on FB-41. |
| Dart / Flutter | Not planned for this release | Skipped by decision on 2026-11-10. |

## 2. JavaScript / TypeScript SDK

### 2.1 Install and create a client

```sh
npm install @forgebase/sdk
```

```ts
import { ForgeBase } from "@forgebase/sdk";

const fb = new ForgeBase("http://localhost:8090");
```

The package has no runtime dependencies. It needs `fetch`, which Node 20+,
browsers, Deno and Bun provide. It ships ESM, CJS and type declarations.

### 2.2 What it covers

| Area | Entry point | Details |
|---|---|---|
| Authentication | `fb.auth` | Sign-up and sign-in, MFA, magic links, one-time codes, OAuth and OIDC redirect flows, sessions in memory or a storage adapter, HttpOnly cookie mode, API keys for servers. See [sdk/js/README.md#authentication](../sdk/js/README.md#authentication). |
| Collections and records | `fb.collection<T>(name)` | Create, read, update with optimistic concurrency, replace, delete, list with filter, sort, expand and fields, and iterators over pages. See [§3](#3-collections-and-records). |
| Realtime | `fb.realtime` | One connection for every subscription. WebSocket with SSE fallback. Automatic resume from the last cursor. Channels, presence and publish. See [sdk/js/README.md#realtime](../sdk/js/README.md#realtime). |
| Entitlements and quotas | `fb.entitlements` | See [sdk/js/README.md#entitlements-and-quotas](../sdk/js/README.md#entitlements-and-quotas). |
| Raw requests | `fb.request(path, options)` | For routes the SDK does not wrap yet, such as passkeys. |

### 2.3 Retries and errors

- `429` and `503` are retried for every method. `502`, `504` and network errors
  are retried only for idempotent methods. A `Retry-After` header is honoured.
- Every failure is a `ForgeBaseError` with `status`, `code`, `message`,
  `requestId`, `details` and `retryAfter`. Subclasses include `ValidationError`,
  `AuthenticationError`, `PermissionError`, `NotFoundError`, `ConflictError`,
  `RateLimitError`, `QuotaExceededError`, `ServerError`, `NetworkError` and
  `RealtimeError`.

See [sdk/js/README.md#retries](../sdk/js/README.md#retries) and
[#errors](../sdk/js/README.md#errors). The error envelope itself is in
[api.md §6](api.md#6-errors).

## 3. Collections and records

```ts
const posts = fb.collection<{ id: string; title: string; views: number }>("posts");

const p = await posts.create({ title: "Hello" });
await posts.update(p.id, { views: 1 }, { version: p._version }); // If-Match; ConflictError if stale
await posts.delete(p.id);

const page = await posts.list({
  filter: 'status = "live" && views > {:min}',
  params: { min: 10 },
  sort: ["-created", "title"],
  page: 2,
  perPage: 50,
});
```

The filter language, sort keys and pagination are described in
[database.md §6](database.md#6-query-language) and
[api.md §7](api.md#7-lists-and-pagination). The SDK's `list` options map onto
those query parameters.

## 4. Types

The SDK's route types are generated from the server's OpenAPI document. The
checked-in copy is `sdk/js/openapi/openapi.json`, and the generated file is
`sdk/js/src/generated/openapi.ts`:

```sh
npm run generate                                 # from the checked-in document
npm run generate -- --url http://127.0.0.1:8090  # refresh the document from a running server
npm run generate:check                           # CI: fails when the generated file is stale
```

Use the types directly:

```ts
import type { components, paths } from "@forgebase/sdk";
```

## 5. Development and tests

```sh
cd sdk/js
npm install
npm run typecheck && npm run lint && npm test && npm run build
```

`npm test` runs the unit and HTTP tests and skips the live tests. The live tests
start the real server binary, seed an admin, and drive the SDK against it:

```sh
cargo build -p forgebase-cli                       # from the repository root
cd sdk/js
FB_LIVE_BIN=$PWD/../../target/debug/forgebase npm run test:live
```

`prepublishOnly` runs typecheck, lint, tests and build before a publish.

## 6. Known limitations

These are listed in [sdk/js/README.md#known-limitations](../sdk/js/README.md#known-limitations).
Two points to check against the current server:

- The README says there is no storage API in the SDK. The server does expose
  storage routes under `/api/v1/storage`.
- The README says `/api/auth/*` is not in the OpenAPI document except OAuth and
  WebAuthn. The server merges the auth document when auth is enabled. Compare
  the live `GET /api/openapi.json` with the SDK's types before you rely on either.

## 7. Versioning

The SDK follows the API's versioning rules. Within `/api/v1` the server only adds
fields, endpoints and optional parameters. Clients must ignore unknown fields
([api.md §2](api.md#2-versioning)). A breaking change to the server means a new
`/api/v2` path and a new SDK major version.
