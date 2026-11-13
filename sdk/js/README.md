# @forgebase/sdk

TypeScript SDK for [ForgeBase](../../README.md): auth, collections and records, storage, key-value, functions, vector search, graph, SQL, realtime,
queues, jobs, billing, entitlements, credits, webhook verification, and the organization, identity and system administration APIs.

- Zero runtime dependencies. Needs `fetch` (Node 20+, browsers, Deno, Bun); `WebSocket` is optional (SSE is the fallback).
- Tree-shakeable (`sideEffects: false`): every area is its own module on the client; the webhook helpers are also at `@forgebase/sdk/webhooks`.
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

> Files live in [Storage](#storage), documents in [Documents](#documents). Collection `export`/`import` are the bulk transfers for records.

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

## Queues and pub/sub

Durable topics with subscriptions, replay and dead letters (`/api/v1/queues`):

```ts
await fb.queues.createTopic("orders");
await fb.queues.createSubscription("billing", "orders", { ackDeadlineMs: 30_000, maxDeliveryAttempts: 5 });
await fb.queues.publish("orders", { id: 1 }, { attributes: { kind: "rush" }, orderingKey: "u1", dedupId: "o-1" });

// Streaming pull: resolve = ack, throw = nack (redelivered after the backoff). Reconnects by itself.
const stream = fb.queues.subscribe<{ id: number }>("billing", async (d) => charge(d.data.id), { maxMessages: 50 });
stream.unsubscribe();

// Manual pull
for (const m of await fb.queues.pull("billing", { maxMessages: 10, waitMs: 2000 })) await fb.queues.ack("billing", [m.ack_id]);
await fb.queues.seek("billing", { time: new Date(Date.now() - 3_600_000) }); // replay the last hour
```

## Webhook verification

A push subscription with a webhook endpoint (`push: { kind: "webhook", endpoint }`) makes the server `POST` every message to your URL as JSON
`{ subscription, delivery_attempt, message }`. Each request carries

```
X-Forgebase-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>
```

signed with the subscription's `whsec_…` secret (returned once, by `queues.createSubscription`). Verify it before you trust the request.
The helpers use Web Crypto, so they run unchanged on Node 18+, Deno, Bun, Cloudflare Workers and Vercel Edge. They need no client.

```ts
import { constructWebhookEvent, verifyWebhookSignature, WebhookVerificationError } from "@forgebase/sdk";
// or, without the rest of the SDK: import { … } from "@forgebase/sdk/webhooks";

const event = await constructWebhookEvent<{ orderId: string }>({
  secret: process.env.FORGEBASE_WEBHOOK_SECRET!,   // a string, or an array while you roll the secret over
  header: request.headers.get("x-forgebase-signature"),
  body: rawBody,                                    // the exact bytes received (string, Uint8Array, Buffer, ArrayBuffer)
  toleranceSeconds: 300,                            // default; also rejects timestamps this far in the future
});
event.subscription; event.delivery_attempt; event.message.data.orderId;
```

- `verifyWebhookSignature(opts)` resolves `true` or `false`. `constructWebhookEvent(opts)` verifies, then returns the typed envelope, or throws a
  `WebhookVerificationError` whose `reason` is `missing_header`, `malformed_header`, `timestamp_out_of_tolerance`, `signature_mismatch` or
  `invalid_payload`. Answer a failure with a 4xx; answer success with a 2xx, anything else makes the server retry with backoff.
- **Use the raw body.** The signature covers the bytes on the wire. A body that was parsed and serialised again no longer matches. Pass a
  parsed object and the helpers throw a `TypeError`.
- **Secret rotation.** While two secrets are valid the server sends `t=…,v1=<new>,v1=<old>`; the check passes when any `v1` matches. On your
  side pass `secret: [newSecret, oldSecret]` to accept both while you switch.
- The comparison is constant time; the tolerance window (default 300 s, `now` can be injected) limits replays. De-duplicate on
  `event.message.id` as well: delivery is at least once.
- `signWebhook({ secret, body, timestamp? })` builds a header, to test your own handler.

**Express** (the raw body must reach the handler, so use `express.raw` on this route and `express.json` elsewhere):

```ts
import express from "express";
import { constructWebhookEvent, WebhookVerificationError } from "@forgebase/sdk";

const app = express();
app.post("/hooks/orders", express.raw({ type: "application/json" }), async (req, res) => {
  try {
    const event = await constructWebhookEvent({
      secret: process.env.FORGEBASE_WEBHOOK_SECRET!,
      header: req.header("x-forgebase-signature"),
      body: req.body,                               // a Buffer, because of express.raw
    });
    await handle(event.message);
    res.sendStatus(204);
  } catch (e) {
    if (e instanceof WebhookVerificationError) return void res.status(400).json({ error: e.reason });
    res.sendStatus(500);                            // the server retries
  }
});
```

**Next.js** (App Router route handler; `request.text()` is the raw body):

```ts
// app/api/forgebase/route.ts
import { constructWebhookEvent, WebhookVerificationError } from "@forgebase/sdk/webhooks";

export async function POST(request: Request) {
  try {
    const event = await constructWebhookEvent({
      secret: process.env.FORGEBASE_WEBHOOK_SECRET!,
      header: request.headers.get("x-forgebase-signature"),
      body: await request.text(),
    });
    await handle(event.message);
    return new Response(null, { status: 204 });
  } catch (e) {
    if (e instanceof WebhookVerificationError) return Response.json({ error: e.reason }, { status: 400 });
    throw e;
  }
}
```

**Hono** (Node, Bun, Deno, Workers):

```ts
import { Hono } from "hono";
import { constructWebhookEvent, WebhookVerificationError } from "@forgebase/sdk/webhooks";

const app = new Hono<{ Bindings: { FORGEBASE_WEBHOOK_SECRET: string } }>();
app.post("/hooks/orders", async (c) => {
  try {
    const event = await constructWebhookEvent({
      secret: c.env.FORGEBASE_WEBHOOK_SECRET,
      header: c.req.header("x-forgebase-signature"),
      body: await c.req.text(),                     // do not use c.req.json()
    });
    await handle(event.message);
    return c.body(null, 204);
  } catch (e) {
    if (e instanceof WebhookVerificationError) return c.json({ error: e.reason }, 400);
    throw e;
  }
});
```

## Jobs and long tasks

```ts
const job = await fb.jobs.enqueue("video.transcode", { id }, { timeoutMs: 3_600_000, idempotencyKey: `t-${id}` });
const sub = await fb.jobs.onProgress(job.id, (u) => console.log(u.status, u.progress?.percent)); // stored state first, then live
const done = await fb.jobs.wait(job.id); // rejects with JobFailedError when the job dies or is cancelled
await sub.unsubscribe();

// Workflows: a DAG of jobs (a step without `after` follows the previous one).
const wf = await fb.jobs.createWorkflow(
  workflow("etl").onFailure("compensate")
    .step("fetch", "http.get", { url }, { compensate: { kind: "cleanup" } })
    .step("parse", "csv.parse")
    .step("load", "db.load", null, { after: ["parse"] }),
);
await fb.jobs.waitWorkflow(wf.id);
```

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

### Administration

Reads need `billing:read`; catalog and override writes need platform administrator rights.

```ts
await fb.entitlements.features.put("api_calls", { kind: "metered", name: "API calls", unit: "call" });
await fb.entitlements.plans.put("pro", { name: "Pro", trialDays: 14, features: [{ feature: "api_calls", limit: 100_000, period: "month" }] });
await fb.entitlements.quotaPolicies.put("api_calls", { enforcement: "soft", overagePct: 10, rolloverPct: 50 });
await fb.entitlements.usagePacks.put("boost", { feature: "api_calls", units: 10_000, validDays: 30 });
await fb.entitlements.usagePacks.grant({ subject: "org:1", pack: "boost", idempotencyKey: "order-9" });

// Per subject: plan sync (idempotent by eventId), enterprise limits, overrides, add-ons, trials
await fb.entitlements.syncSubscription({ subject: "org:1", plan: "pro", status: "active", eventId: "evt_1", eventAt: new Date().toISOString() });
await fb.entitlements.limits.put({ subject: "org:1", feature: "api_calls", limit: 5_000_000, period: "month" });
await fb.entitlements.overrides.add({ subject: "org:1", feature: "sso", effect: "allow", reason: "pilot" });
await fb.entitlements.setAddon({ subject: "org:1", addon: "seats_5", quantity: 2 });
await fb.entitlements.startTrial({ subject: "org:1", plan: "pro", days: 14 });

// Usage: gauges (storage, seats), the immutable event feed, alerts
await fb.entitlements.adjustGauge({ feature: "seats", delta: 1, idempotencyKey: "invite-42" });
let page = await fb.entitlements.usageEvents({ subject: "org:1", limit: 500 });
while (page.next != null) page = await fb.entitlements.usageEvents({ subject: "org:1", after: page.next });
const alerts = await fb.entitlements.usageAlerts("org:1");
```

Namespaces: `plans` and `features` (`list`, `get`, `put`, `delete`), `addons` and `quotaPolicies` (`list`, `put`, `delete`), `usagePacks`
(catalog `list`, `put`, `delete`; held packs `grants`, `grant`), `overrides` (`list`, `add`, `delete`), `limits` (`list`, `put`, `delete`).

## Storage

`fb.storage`: buckets, objects, multipart and resumable uploads, signed URLs, versions.

```ts
await fb.storage.buckets.create({ name: "avatars", visibility: "private", versioning: true, allowedContentTypes: ["image/*"] });
const avatars = fb.storage.from("avatars");

await avatars.put("u1/me.png", file, { contentType: "image/png", metadata: { owner: "u1" }, checksumSha256: await sha256Hex(file) });
const res = await avatars.download("u1/me.png", { range: { start: 0, end: 1023 } }); // raw Response, 206 for a range
const text = await avatars.text("notes/a.txt"); // also json(), blob(), bytes()
for await (const o of avatars.iterate({ prefix: "u1/" })) console.log(o.key, o.size);

await avatars.copy("u1/me.png", "u1/old.png");
const versions = await avatars.versions("u1/me.png");
await avatars.restore("u1/me.png", versions[1]!.version_id);

// signed URLs: usable without credentials until they expire
const { url } = await avatars.createSignedUrl("u1/me.png", { expiresIn: 600, downloadAs: "me.png" });
const link = fb.storage.signed.resolve(url); // absolute URL
const up = await avatars.createSignedUploadUrl("u1/new.png", { contentType: "image/png" });
await fb.storage.signed.upload(up, file, { contentType: "image/png" });

// big files: multipart in 8 MiB parts (aborts the session on failure) or resumable
await fb.storage.uploads.upload("videos", "clip.mp4", blob, { partSize: 16 << 20, onProgress: (p) => console.log(p.uploadedBytes / p.totalBytes) });
const s = await fb.storage.uploads.create("videos", "big.mov", { kind: "resumable", size: blob.size });
await fb.storage.uploads.resume(s.id, blob); // call again after a failure: continues at the server's offset
```

Bodies are `Blob`, `ArrayBuffer`, typed arrays or strings. `ReadableStream` bodies are not supported (Node needs `duplex: "half"`): use
`uploads.upload` or `uploads.resume` for large data. Administration: `buckets.setAcl`, `buckets.usage`, `storage.stats()`,
`storage.reconcile()`, `storage.rotateSigningKey()`. `sha256Hex(bytes)` computes the optional `checksumSha256`.

## Documents

`fb.documents.collection<T>(name)`: the document form of the record operations, with merge patch and `If-Match` versions.

```ts
const notes = fb.documents.collection<Note>("notes");
const n = await notes.create({ title: "Hi", tags: ["a"] });
await notes.patch(n.id, { tags: null }, { version: n._version }); // RFC 7386; a stale version throws ConflictError
const history = await notes.versions(n.id);                       // collections with versioned: true
```

## Key-value store

`fb.kv` is the Redis-style engine: strings, hashes, lists, sets, sorted sets, streams, TTLs and pub/sub, all inside a namespace. Key
operations go through a namespace handle.

```ts
const cache = fb.kv.ns("cache");

await cache.set("session:42", JSON.stringify({ uid: 42 }), { ttlMs: 60_000 });
const raw = await cache.text("session:42");          // string | null
await cache.incr("hits");                            // 1
await cache.hashSet("user:1", { name: "Ada", plan: "pro" });
await cache.listPush("jobs", "right", ["a", "b"]);
await cache.zsetAdd("board", [{ member: "ada", score: 10 }]);
await cache.streamAdd("events", { type: "login" }, { maxLen: 1000 });

const k = await cache.get("user:1");                 // { type: "hash", ttl_ms, size, truncated, value }
for await (const key of cache.keys({ match: "user:*" })) console.log(key);

await cache.exec(["ZRANGE", "board", 0, -1, "WITHSCORES"]);   // any command, as an array
await cache.batch([["SET", "a", "1"], ["INCR", "a"]]);         // 1 to 1000 commands, one `{result}|{error}` each

const sub = cache.subscribe("news", (m) => console.log(m.message));
await cache.publish("news", "hello");
sub.unsubscribe();

await fb.kv.namespaces();                            // [{ name, keys }]
await fb.kv.flush("cache");                          // deletes every key, returns the count
```

Bytes that are not valid UTF-8 come back as `{ base64 }`; decode with `kvBytes` / `kvText`. `get`, `expire` and `persist` throw
`NotFoundError` for a missing key. Pub/sub messages are not stored: a subscriber only sees what is published while its stream is open
(pass `{ reconnect: false }` if a silent gap on reconnect matters). `streamRead` does not block; poll with the last id you saw.

## SQL, indexes and slow queries

```ts
const rows = await fb.sql.query<{ id: string; title: string }>("SELECT id, title FROM posts WHERE views > ?", [100]);
const r = await fb.sql.execute("UPDATE posts SET views = views + 1 WHERE id = ?", ["p1"]);       // r.rows_affected
await fb.sql.batch(["DELETE FROM tmp", { sql: "INSERT INTO tmp(a) VALUES (?)", params: [1] }]);  // one transaction
const plan = await fb.sql.explain("SELECT * FROM posts WHERE title = ?", ["x"]);                 // plan.full_scans
const tables = await fb.sql.tables();

await fb.indexes.create({ collection: "posts", keys: ["author", { path: "created", desc: true }] });
await fb.indexes.create({ collection: "posts", keys: ["body"], kind: "fulltext" });             // or "trigram" (PostgreSQL pg_trgm)
await fb.indexes.rebuild("ix_posts_author");
await fb.indexes.setSlowQueryThreshold(50);                  // ms; 0 logs every statement
const { items } = await fb.indexes.slowQueries();
const suggestions = await fb.indexes.recommendations();
```

SQL is for administrators by default and takes positional `?` parameters. Secret columns come back as `********` unless
`{ showSecrets: true }` (audited). Reads return at most 10 000 rows (`truncated`). `readFromReplica`, `set` and `runAs` (PostgreSQL) go in the
options argument. A taken index name or duplicate values under a unique index throw `ConflictError`.

## Graph

`fb.graph` is a property graph with typed edges, traversal, shortest path and read-only openCypher. Responses keep the server's snake_case.

```ts
await fb.graph.nodes.create({ id: "alice", label: "User", properties: { country: "IN" } });
await fb.graph.nodes.create({ id: "bob", label: "User" });
await fb.graph.edges.create({ type: "FOLLOWS", source: "alice", target: "bob" });

const { items, nextAfter } = await fb.graph.nodes.list({ label: "User", limit: 100 });
for await (const page of fb.graph.nodes.pages({ label: "User" })) console.log(page.items.length);

await fb.graph.nodes.patch("alice", { properties: { city: "Pune", old: null } }); // RFC 7396 merge, null removes a key
const { items: hops } = await fb.graph.nodes.neighbors("alice", { direction: "both", types: ["FOLLOWS"] });
const reach = await fb.graph.traverse({ start: "alice", maxDepth: 3, edgeTypes: ["FOLLOWS"], filter: { label: "User" } });
const path = await fb.graph.shortestPath({ from: "alice", to: "zed" });   // path.found === false when none
await fb.graph.nodes.delete("alice");                                     // also deletes its edges: { id, deleted_edges }

const r = await fb.graph.cypher("MATCH (u:User {id: $id})-[:FOLLOWS]->(f) RETURN f", { params: { id: "alice" }, limit: 50 });
await fb.graph.dropCypher();    // drops the derived openCypher graph only
```

openCypher is read-only and needs PostgreSQL with Apache AGE and the `engine:graph_cypher` permission. The first query of a tenant builds
the graph and fails with a `ConflictError` (code `graph_materialising`, `err.body.error.job_id`); retry once
`(await fb.graph.stats()).cypher?.state === "ready"`.

## Vector search

`fb.vector` holds collections with one or more vector fields (dimension plus metric `cosine`, `l2` or `dot`). Items carry vectors, JSON
metadata and optional text.

```ts
await fb.vector.create({ name: "docs", fields: [{ name: "embedding", dimension: 1536, metric: "cosine", quantization: "f16" }] });
const docs = fb.vector.collection("docs");

await docs.upsert([{ id: "a", vectors: { embedding }, metadata: { category: "news", year: 2024 }, text: "ForgeBase ships…" }]); // 1–1000 items

const { hits, strategy } = await docs.search({
  vector: queryEmbedding, k: 5,
  filter: "category = {:c} && year >= 2020", params: { c: "news" },
});
// hit: { id, distance (lower is closer), score (higher is closer), metadata }

const mixed = await docs.hybrid({ vector: queryEmbedding, text: "rust database", k: 10, weights: { vector: 1, text: 0.5 } });
// Reciprocal Rank Fusion: hit.score, vector_rank, text_rank. Leave out `vector` for keyword-only search, or `text` for vector-only.

await docs.deleteItems(["a", "b"]);                                   // { deleted }
await docs.fields.add({ name: "title_vec", dimension: 384, metric: "l2" });
await docs.rebuild();                                                 // background ANN rebuild; progress in (await docs.stats()).indexes
const { recall } = await docs.recall({ k: 10, samples: 100 });
```

`filterMode` is `auto` (default), `pre` (filter first, exact when few items match) or `post` (search first, then widen). `exact: true` forces
brute force.

## Functions

`fb.functions` deploys, inspects, rolls back and invokes server functions. Managing needs the `manage_functions` permission, reading
`read_functions`; invoking depends on the function's `access` (`public`, `authenticated`, `permission`).

```ts
const { function: fn, version } = await fb.functions.deploy({
  name: "hello",
  language: "typescript",
  source: `import { serve } from "forgebase"; serve(async (req) => ({ body: { hello: req.query.name ?? "world" } }));`,
  access: "authenticated",
  capabilities: ["kv", "fetch"],
  fetchHosts: ["api.example.com"],
  limits: { timeoutMs: 2000 },
});

await fb.functions.invoke("hello", undefined, { method: "GET", query: { name: "ada" } }); // { hello: "ada" }
await fb.functions.invoke("api", { a: 1 }, { path: "users/42" });  // POST .../invoke/users/42
const res = await fb.functions.invokeRaw("report", { id: 1 });      // Response: headers, binary, streams

await fb.functions.versions("hello");
await fb.functions.rollback("hello");             // newest version before the active one; pass a number to pick
await fb.functions.logs("hello", { limit: 20 });
await fb.functions.secrets.set("hello", "API_KEY", "…");
await fb.functions.kvRules.set("hello", [{ namespace: "app.*", access: "read" }]); // administrators
const dts = await fb.functions.types();           // TypeScript declarations of the "forgebase" module
```

`invoke` sends JSON by default (a string, `Blob`, `ArrayBuffer`, typed array, `FormData`, `URLSearchParams` or stream goes out as is) and
parses a JSON answer. A source that does not compile throws `ValidationError` (`function_compile_failed`; `err.details` has `line`,
`column`, `message`). A function that answers with an error status raises the matching typed error (its JSON body is on `err.body`). `invoke`
defaults to `POST`; pass `retry: false` for non-idempotent functions called with another verb.

## Credits

`fb.credits` holds prepaid credit balances on an append-only ledger. `subject` is `org:<id>` or a principal key (default: the caller). Every
mutation takes an `idempotencyKey`; a replay returns the first result with `replayed: true`.

```ts
const { balance, expiring } = await fb.credits.balance({ subject: "org:42" });

await fb.credits.topUp({ subject: "org:42", amount: 500, idempotencyKey: "pay_123", externalRef: "pi_9" });   // operators
await fb.credits.grant({ subject: "org:42", amount: 50, idempotencyKey: "welcome", expiresAt: new Date("2027-01-01") });

try {
  await fb.credits.spend({ subject: "org:42", amount: 5, feature: "ai.tokens", units: 1000, idempotencyKey: "job_77" });
} catch (e) {
  if (e instanceof ApiError && e.code === "insufficient_credits") console.log(e.details); // { balance, requested }
}

await fb.credits.refund({ entryId: "le_…", idempotencyKey: "refund_1", amount: 3 });
await fb.credits.reverse({ entryId: "le_…", idempotencyKey: "rev_1" });   // operators: takes back what is left of a top-up or grant

for await (const e of fb.credits.entries({ kind: "consumption", from: "2026-01-01T00:00:00Z" })) console.log(e.seq, e.amount);
const page = await fb.credits.ledger({ limit: 100, after: 0 });           // page.next is the cursor for the next page

await fb.credits.stats.summary({ from: new Date(Date.now() - 7 * 864e5) });
await fb.credits.rates.set("ai.tokens", { creditsPerUnit: 2, perUnits: 1000 });  // platform administrators
```

Also: `lots`, `entry`, `audit`, `usage`, `usageCursor.get/set`, `stats.balance/consumption`, `maintenance`.

## Billing

`fb.billing` covers customers, subscriptions, one-time orders, invoices and the customer portal. The catalog, providers, settings and operator
views need platform administrator rights. Store purchases are under `fb.billing.store`.

```ts
// Subscribe. A replayed idempotency key returns the first result with `replayed: true`.
const out = await fb.billing.subscriptions.create({
  idempotencyKey: crypto.randomUUID(),
  items: [{ price: "pro_month", quantity: 1 }],
  trialDays: 14,
  returnUrl: "https://app.example.com/billing/done",
});
if (out.payment_url) location.href = out.payment_url;     // the customer still has to pay

// Plan change: preview the proration first, then apply it.
const preview = await fb.billing.subscriptions.previewChange(sub.id, { price: "team_month" });
console.log(preview.credit, preview.charge, preview.net);
await fb.billing.subscriptions.change(sub.id, { price: "team_month" });
await fb.billing.subscriptions.cancel(sub.id, { atPeriodEnd: true, reason: "too expensive" });

// Portal: invoices, payments, saved payment method.
const portal = await fb.billing.portal.get();
const { url } = await fb.billing.portal.paymentMethod({ returnUrl: location.href });
const open = await fb.billing.invoices.list({ status: "open" });
await fb.billing.invoices.pay(open[0].id, { returnUrl: location.href });

// Operators: catalog, credit notes, statistics.
await fb.billing.catalog.products.put("pro", { name: "Pro", kind: "plan", planKey: "pro" });
await fb.billing.catalog.prices.put("pro_month", { product: "pro", type: "recurring", currency: "usd", unitAmount: 1900, interval: "month" });
await fb.billing.invoices.creditNote(invoiceId, { idempotencyKey: "cn-1", amount: 500, toBalance: true });
const stats = await fb.billing.stats({ interval: "month" });     // MRR, ARR, churn, revenue
const dunning = await fb.billing.dunning();                      // open invoices being retried
```

Options are camelCase and map to the server's snake_case one level deep; nested objects (`items`, `tiers`, `currencyOptions`, `metadata`)
keep the wire shape.

| Surface | Methods |
|---|---|
| `customers` | `get`, `put`, `list` |
| `portal` | `get`, `paymentMethod` |
| `subscriptions` | `list`, `get`, `create`, `change`, `previewChange`, `cancel`, `pause`, `resume` |
| `orders` | `list`, `get`, `create`, `listAll` |
| `invoices` | `list`, `get`, `pay`, `void`, `markPaid`, `creditNote` |
| `creditNotes`, `payments`, `refunds` | `list({ subject, limit })` |
| `catalog.products`, `catalog.prices` | `list`, `get`, `put`, `archive` |
| `catalog.coupons`, `catalog.taxRates` | `list`, `put`, `archive` |
| `providers`, `settings` | `list`/`get`, `put`, `delete` (providers) |
| `store` | `verify`, `checkout`, `purchases`, `products`, `putProduct`, `deleteProduct` |
| top level | `stats`, `usageMeters`, `dunning`, `maintenance` |

## Organizations and teams

`fb.orgs` covers the tenancy routes: organizations, projects, members, invitations, teams, roles and role sets. Lists are plain arrays. Ids
(not slugs) address orgs, projects and teams. Methods with an optional `org` act on the realm when it is left out.

```ts
const org = await fb.orgs.create({ slug: "acme", name: "Acme" });        // the caller becomes owner
await fb.orgs.rename(org.id, "Acme Inc");

const project = await fb.orgs.projects.create({ slug: "web", name: "Web" }, org.id);
await fb.orgs.projects.createEnvironment(project.id, "staging");

await fb.orgs.roles.put({ key: "dev", allow: ["records:*"], deny: ["billing:*"] }, org.id);
await fb.orgs.roleSets.put({ key: "eng", name: "Engineering", roles: ["dev"] }, org.id);
await fb.orgs.members.set(org.id, { principal: "user:users/u1", roles: ["dev"] });
await fb.orgs.members.setActive(org.id, "user:users/u1", false);         // suspend the membership

const eng = await fb.orgs.teams.create(org.id, { key: "eng", name: "Engineering", roles: ["dev"] });
await fb.orgs.teams.addMember(eng.id, "user:users/u1");

const inv = await fb.orgs.invitations.create(org.id, "ann@example.com", ["viewer"]);   // the token is shown once
await fb.orgs.invitations.accept(inv.token);                                           // as the invited, signed-in user
```

Platform super-admins also have `orgs.realms`, `orgs.realmBindings` and `orgs.suspensions` (`suspend(principal, { reason, until })`, `lift`).

## Access control

`fb.access` dry-runs permission checks and manages ACLs, overrides and rules.

```ts
if (await fb.access.can("projects:update", { org: org.id })) { /* ... */ }
const d = await fb.access.check({ permission: "records:delete", resourceType: "project", resourceId: project.id });
if (!d.allowed) console.log(d.step, d.reason);
const me = await fb.access.me(org.id);                                    // roles, allow, deny, teams

// Per-resource ACL: grantee is a principal, `team:<id>` or `role:<key>`
await fb.access.acl.put({ resourceType: "project", resourceId: project.id, grantee: `team:${eng.id}`, permission: "projects:read", effect: "allow" });

// Collection rules (super-admins): omitted or null = locked, "" = everyone
await fb.access.policies.put("posts", { listRule: "", viewRule: "", createRule: "@request.auth.id != ''" });

// Engine resource rules (KV namespaces, buckets, ...): "*" is the default for the kind
await fb.access.resourceRules.put("kv", "sessions", { read: "", write: null });
const r = await fb.access.resourceRules.check("kv", "sessions", { action: "write", op: "update" });
```

Also `acl.list/delete`, `overrides.list/add/delete`, `policies.get/delete`, `resourceRules.list/get/delete`, `audit(limit)`. Most policy, realm and
audit routes need a super-admin and otherwise throw `PermissionError`.

## Identity administration

`fb.identity` covers what an admin console or settings page needs beyond sign-in. Sign-in, sessions and `auth.createToken` stay on `fb.auth`.
Admin calls need an administrator or an API key with the matching scope.

```ts
// Machine credentials
const sa = await fb.identity.serviceAccounts.create({ name: "ci", role: "service" });
const { key } = await fb.identity.serviceAccounts.keys.create(sa.id, { name: "deploy", scopes: ["records:write"], expiresInDays: 90 }); // shown once
await fb.identity.apiKeys.revoke(keyId);               // also apiKeys.list(), tokens.list() / revoke()

// OAuth / OIDC client apps
const { client, client_secret } = await fb.identity.clients.create({ name: "Portal", redirectUris: ["https://portal.example/cb"] });
await fb.identity.clients.rotateSecret(client.client_id, { graceSeconds: 3600 });   // the old secret keeps working for an hour
fb.identity.oidc.discoveryUrl("default");              // hand this to an OIDC library

// Realms: methods, signup, social providers, SMS gateway
await fb.identity.realms.update("acme", { config: { signup: false, methods: { password: true, passkey: true } } });
await fb.identity.realms.providers.put("acme", "github", { kind: "github", client_id: "…", client_secret: "…" });

// Users: lifecycle, sessions, export, support impersonation, audit
await fb.identity.users.suspend("users", userId, { reason: "abuse", until: new Date(Date.now() + 7 * 864e5) });
await fb.identity.users.revokeSessions("users", userId);
const grant = await fb.identity.impersonation.start({ principal: `user:users/${userId}`, reason: "ticket 4711", ttlMinutes: 10 });
const events = await fb.identity.audit({ event: "login.failed", limit: 50 });

// Own account security
const { otpauth_url } = await fb.identity.mfa.setupTotp();                 // show as QR, then:
const { recovery_codes } = await fb.identity.mfa.confirmTotp("123456");
const opts = await fb.identity.webauthn.registerOptions({ useAsMfa: true });
// navigator.credentials.create({ publicKey: opts.publicKey }) → fb.identity.webauthn.registerVerify({ ceremonyId: opts.ceremony_id, credential })
await fb.identity.phone.signIn({ phone: "+4915112345678", code: "123456" });  // also webauthn.signIn, ldap.signIn
await fb.identity.account.export();                                          // account.deactivate() / account.delete({ anonymise: true })
```

The OIDC protocol endpoints (`authorize`, `token`, `userinfo`, ...) are for OIDC libraries; only `oidc.issuer`, `discoveryUrl`, `discovery` and
`jwks` are wrapped. Passkey `credential` payloads are passed through as is: serialise the browser's `PublicKeyCredential` yourself.

## Customization

`fb.customize` handles per-realm domains, branding, email and legal text. Platform administrators can pass `{ realm }` to act on another realm.

```ts
const d = await fb.customize.domains.add("login.acme.test");     // publish d.verification (a DNS TXT record), then:
await fb.customize.domains.verify(d.id);                          // { verified, reason? }

await fb.customize.branding.put({ display_name: "Acme", primary_color: "#0a84ff" });
await fb.customize.branding.uploadAsset("logo", file);            // Blob / Uint8Array, up to 256 KiB (pass contentType for non-PNG bytes)

await fb.customize.email.putTemplate("magic_link", { subject: "Sign in to {{app}}", text: "Open {{link}}" });
await fb.customize.email.send({ template: "invitation", to: "new@acme.test", variables: { link } });
const failed = await fb.customize.email.deliveries({ status: "failed", limit: 20 });
await fb.customize.email.retry(failed[0].id);

await fb.customize.agreements.publish({ kind: "terms", title: "Terms of Service", body: "…" });
const todo = await fb.customize.agreements.pending();              // [{ kind, version, title }]
await fb.customize.agreements.accept(todo.map(({ kind, version }) => ({ kind, version })));

// Hosted sign-in page (no credentials sent)
const brand = await fb.customize.hosted.branding();
fb.customize.hosted.assetUrl("logo");  fb.customize.hosted.themeUrl();  fb.customize.docsUrl();
```

## System administration

`fb.system` (administrators only): features and config, database topology, backups, DB logins, extensions.

```ts
const { features } = await fb.system.features();
await fb.system.updateConfig({ "storage.max_object_bytes": 104857600 });   // applies after restart

const db = fb.system.database;
const op = await db.addNode({ kind: "local", name: "replica-2" });
await db.waitForOperation(op.operation!.id, { timeoutMs: 600_000 });
await db.promote(nodeId, { confirm: "replica-2" });                        // may throw PermissionError "mfa_step_up_required"

await fb.system.backups.create({ label: "nightly", retention: { keepLast: 7 } });
const { password } = await fb.system.dbLogins.create({ name: "etl", readonly: true });   // shown once
```

Topology-changing database calls take a typed `confirm` value (node, shard or tenant name, or the plan id) and run as operations you can poll
or cancel. `database.agent.*` is for the `forgebase node` agent and is untyped.

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
| `WebhookVerificationError` | a push request failed `constructWebhookEvent` (`.reason`) |
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

- The OpenAPI document does not describe `/api/auth/*` (except OAuth and WebAuthn), nor most of the policy, orgs, realm and customization bodies, and it
  omits query parameters everywhere; those types are hand-written from the server handlers (snake_case, like the wire). Compare with the live
  `GET /api/openapi.json` after a server upgrade.
- Passkeys (WebAuthn) are wrapped as option/verify calls only: the browser credential APIs and the `PublicKeyCredential` serialisation are yours.
- `Transport` throws on every non-2xx status, so a function answering 404/500 on purpose surfaces as a typed error (JSON body on `err.body`), and
  conditional reads (`If-None-Match` to a 304) are not supported.
- `ReadableStream` request bodies are not supported (Node needs `duplex: "half"`); use the multipart/resumable upload helpers for large files.
- Webhook-endpoint tooling (attempt log, test send, redrive, secret rotation, endpoint health, event catalog) and the billing webhook inspector
  arrive with the server endpoints; `fb.queues` and the verification helpers above cover receiving webhooks today.
- OIDC protocol endpoints (`authorize`, `token`, `userinfo`, ...) are left to OIDC libraries; `fb.identity.oidc` only builds discovery URLs.
- The server declares duplicate `operationId`s (`list`, `stats`, ...); the generator renames the repeated ones.
