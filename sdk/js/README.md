# @forgebase/sdk

TypeScript SDK for [ForgeBase](../../README.md): auth, collections and records, realtime, entitlements and quotas.

- Zero runtime dependencies. Needs `fetch` (Node 20+, browsers, Deno, Bun); `WebSocket` is optional (SSE is the fallback).
- ESM + CJS + type declarations. Strict TypeScript.
- Types for every route are generated from the server's OpenAPI document and checked in (`src/generated/openapi.ts`).
- License: MIT OR Apache-2.0.

```sh
npm install @forgebase/sdk
```

```ts
import { ForgeBase } from "@forgebase/sdk";

const fb = new ForgeBase("http://localhost:8080");
await fb.auth.signIn({ email: "ada@example.com", password: "..." });

const posts = fb.collection<{ id: string; title: string; views: number }>("posts");
const page = await posts.list({ filter: "views > {:min}", params: { min: 100 }, sort: "-views", perPage: 20 });
```

## Authentication

Every sign-in flow stores the session in the client, refreshes the access token before it expires (one refresh for any number of
concurrent requests), and retries once after a `401`. Pass a `storage` adapter to keep the session across reloads.

```ts
import { ForgeBase, isMfaChallenge, webStorage } from "@forgebase/sdk";

const fb = new ForgeBase(url, { storage: webStorage(localStorage) });

await fb.auth.signUp({ email, password });                    // creates the user (no session)
const r = await fb.auth.signIn({ email, password });          // password
if (isMfaChallenge(r)) await fb.auth.verifyMfa(r.mfaToken, totpCode);

await fb.auth.requestMagicLink(email);                        // magic link: pass the token from the mailed link
await fb.auth.confirmMagicLink(token);
await fb.auth.requestOtp(email);                              // one-time code
await fb.auth.verifyOtp({ email, code });

// OAuth / OIDC redirect flow
window.location.assign(fb.auth.oauth.redirectUrl("github", { cookie: true }));
// ...on your callback page:
await fb.auth.oauth.completeFromUrl("github", window.location.href);

// Servers and scripts: an API key, service-account key or personal access token
const svc = new ForgeBase(url, { apiKey: process.env.FORGEBASE_API_KEY });

fb.auth.onChange((event, session) => {});                     // signed_in | token_refreshed | signed_out
await fb.auth.signOut();
```

Browser apps can use an HttpOnly session cookie instead of tokens: `signIn({ ..., cookie: true })`. The SDK then sends
`credentials: "include"` and the CSRF header on writes.

## Collections and records

```ts
const posts = fb.collection<Post>("posts");

const p = await posts.create({ title: "Hello" });
await posts.update(p.id, { views: 1 }, { version: p._version });   // optimistic concurrency (If-Match), else ConflictError
await posts.replace(p.id, { title: "New", views: 0 });
await posts.delete(p.id);

await posts.list({
  filter: 'status = "live" && views > {:min}',   // filter language; `{:name}` values go in `params`
  params: { min: 10 },
  sort: ["-created", "title"],
  expand: ["author"],
  fields: ["title", "views"],
  page: 2, perPage: 50,
});

// Pagination iterators: follow cursors (or page numbers) lazily
for await (const post of posts.iterate({ filter: "views > 0", perPage: 200 })) { /* ... */ }
for await (const page of posts.pages({ perPage: 100 })) { /* page.items, page.nextCursor */ }
const everything = await posts.all();

await posts.count('status = "draft"');
await posts.first('title = "Hello"');
const ndjson = await posts.export("ndjson");     // download
await posts.import("ndjson", ndjson);            // upload

await fb.collections.create({ name: "tags", type: "base", fields: [{ name: "label", type: "text" }] });
```

Anything without a dedicated helper is one call away and still typed against the OpenAPI document:

```ts
const info = await fb.request<{ version: string }>("/api/system/info");
```

> Object storage routes (FB-30) are not in the server yet, so the SDK has no file upload/download API. Collection `export`/`import`
> are the only bulk transfers today; `fb.request` with `rawBody`/`raw: true` covers new routes until a `files` helper lands.

## Realtime

One connection carries every subscription. WebSocket is used when available and the SDK falls back to Server-Sent Events when the
socket cannot connect (or when `transport: "sse"`). Reconnects are automatic (exponential backoff with jitter) and **resume from the
last cursor seen per subscription**, so no event is lost or delivered twice.

```ts
const sub = await fb.realtime.subscribe({ collection: "posts", filter: 'status = "live"' }, (e) => {
  if (e.type === "record.created") console.log(e.record_id, e.data);
  if (e.type === "reset") reloadEverything();     // cursor too old: reload, then keep listening
});

sub.cursor;               // highest cursor delivered; persist it to resume after a page reload:
await fb.realtime.subscribe({ collection: "posts", cursor: saved }, handler);

// Channels, presence (WebSocket only), publish
await fb.realtime.subscribe({ channel: "room:1" }, (e) => e.type === "message" && console.log(e.data));
await fb.realtime.publish("room:1", { hello: "world" });
const members = await fb.realtime.joinPresence("room:1", { name: "Ada" });

fb.realtime.on("reconnecting", (attempt, delayMs) => {});
fb.realtime.on("transport", (t) => {});          // "websocket" | "sse"
await sub.unsubscribe();
fb.realtime.close();
```

The access token rotates while the socket stays open: the SDK forwards each new token with the protocol's `auth` message, and
reconnects with a fresh one after the server closes with `4001`. Runtimes without a global `WebSocket` (Node < 22) take
`new ForgeBase(url, { realtime: { WebSocket } })` with the `ws` package.

## Entitlements and quotas

```ts
if (await fb.entitlements.has({ feature: "beta_ui" })) { /* ... */ }
await fb.entitlements.has({ permission: "org:billing:read" });
await fb.entitlements.has({ plan: "pro", subject: "org:acme" });

const grants = await fb.entitlements.get();                    // plan + grants by feature
const usage = await fb.entitlements.usage();
const d = await fb.entitlements.checkQuota({ feature: "ai_calls", units: 5 });   // no side effects

await fb.entitlements.consume({ feature: "ai_calls", idempotencyKey: crypto.randomUUID() });  // QuotaExceededError past a hard limit

// Hold units while work runs: committed on success, released on failure
await fb.entitlements.withReservation({ feature: "ai_calls", units: 2, idempotencyKey }, async () => runModel());

const lease = await fb.entitlements.acquireLease({ feature: "concurrent_jobs", idempotencyKey });
await fb.entitlements.renewLease(lease.id); await fb.entitlements.releaseLease(lease.id);
```

Recording usage (`consume`, `reserve`, leases) needs the `usage:write` permission; checks do not.

## Retries

`429` and `503` are retried for every method (the server refused before doing work), `502`/`504` and network errors only for
idempotent methods (GET, PUT, DELETE). A `Retry-After` header (seconds or HTTP date) is honoured; otherwise the delay is an
exponential backoff with full jitter. A `Retry-After` longer than `maxRetryAfterMs` fails immediately instead of sleeping.

```ts
new ForgeBase(url, { retry: { maxRetries: 5, baseDelayMs: 200, maxDelayMs: 5000, maxRetryAfterMs: 30_000 } });
new ForgeBase(url, { retry: { maxRetries: 0 } });             // off
await fb.request("/x", { retry: false, signal: AbortSignal.timeout(5000) });   // per call
```

## Errors

Every failure is a `ForgeBaseError` with `status` (0 = no response), `code`, `message`, `requestId`, `details` and `retryAfter`.

| Class | When |
|---|---|
| `ValidationError` | 400/422; `.fields` has per-field `{code, message}` |
| `AuthenticationError` | 401 |
| `PermissionError` | 403 (policy, scope, CSRF) |
| `NotFoundError` | 404 |
| `ConflictError` | 409/412 (unique, stale `version`) |
| `RateLimitError` | 429 after retries |
| `QuotaExceededError` | 429 because a quota is exhausted (never retried) |
| `ServerError` | 5xx after retries |
| `NetworkError` | no response |
| `RealtimeError` | socket/stream failures (`.code`: `forbidden`, `unsupported_transport`, ...) |

```ts
try { await posts.create({}); }
catch (e) { if (e instanceof ValidationError) console.log(e.fields.title?.message, e.requestId); }
```

## Generated types

`src/generated/openapi.ts` is produced from `openapi/openapi.json` (a copy of `GET /api/openapi.json`):

```sh
npm run generate                                   # from the checked-in document
npm run generate -- --url http://127.0.0.1:8080    # refresh the document from a running server first
npm run generate:check                             # CI: fails when the generated file is stale
```

Use them directly: `import type { components, paths } from "@forgebase/sdk"`.

## Development

```sh
npm install
npm run typecheck && npm run lint && npm test && npm run build

# integration tests against a real server (boots the binary, seeds an admin, drives the SDK)
cargo build -p forgebase-cli                       # from the repo root (see the root README)
FB_LIVE_BIN=$PWD/../../target/debug/forgebase npm run test:live
```

## Known limitations

- No file/object storage API yet (FB-30 has not landed).
- The OpenAPI document does not describe `/api/auth/*` (except OAuth and WebAuthn), so those request/response types are hand-written.
  Passkeys (WebAuthn) are not wrapped: they need browser credential APIs; call the routes with `fb.request`.
- The server declares duplicate `operationId`s (`list`, `stats`, ...); the generator renames the repeated ones.
