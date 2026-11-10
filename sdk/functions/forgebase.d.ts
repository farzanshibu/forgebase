// Type declarations of the ForgeBase function API (ADR-0027).
//
// A function is one ES2023 module that imports only "forgebase" and calls
// serve() once. The server serves this file at GET /api/v1/functions/types
// and `forgebase functions new` writes it next to index.ts.
// The Web globals (Request, Response, Headers, URL, URLSearchParams,
// TextEncoder, TextDecoder, Blob, AbortController, atob, btoa, crypto,
// structuredClone, setTimeout, fetch, console) come from TypeScript's DOM
// library; console output becomes the execution's log lines and fetch needs
// the `fetch` capability.

declare module "forgebase" {
  /**
   * A failed ForgeBase call. `code` is the stable error code
   * (`capability_denied`, `forbidden`, `record_not_found`, ...).
   * @example
   * try { await ctx.db.from("todos").get(id); }
   * catch (e) { if (e instanceof ForgeBaseError && e.code === "record_not_found") return new Response("no", { status: 404 }); throw e; }
   */
  export class ForgeBaseError extends Error {
    /** Stable error code. @example if (e.code === "forbidden") return new Response("nope", { status: 403 }); */
    readonly code: string;
  }

  /** Who is calling (or, from `ctx.verifyToken`, who owns a token). */
  export interface AuthInfo {
    /** Record, administrator or service-account id. @example const owner = ctx.auth?.id; */
    readonly id: string | null;
    /** Kind of caller. @example if (ctx.auth?.type === "admin") { ... } */
    readonly type: "user" | "admin" | "service" | "system";
    /** Auth collection of a user (`users`), else null. @example ctx.auth?.collection === "users" */
    readonly collection: string | null;
    /** RBAC roles held for this request. @example ctx.auth?.roles.includes("editor") */
    readonly roles: readonly string[];
    /** Granted permission patterns (`data:posts:read`, `engine:kv_read`, `*`). @example ctx.auth?.permissions.includes("billing:manage") */
    readonly permissions: readonly string[];
    /** Realm of the request. @example const realm = ctx.auth?.realm; */
    readonly realm: string | null;
    /** Active organization, if any. @example const org = ctx.auth?.org; */
    readonly org: string | null;
    /** Tenant (project environment) the request is scoped to. @example const tenant = ctx.auth?.tenant; */
    readonly tenant: string | null;
    /**
     * Verifies a bearer token (access token, API key or personal access token); `null` when invalid.
     * @example const who = await ctx.auth?.verify(req.headers.get("authorization") ?? "");
     */
    verify(token: string): Promise<AuthInfo | null>;
  }

  /** Options of `list()`. */
  export interface ListOptions {
    /** Filter in ForgeBase's filter language. @example { filter: "done = false && owner = {:me}", params: { me: ctx.auth?.id } } */
    filter?: string;
    /** Values for `{:name}` parameters of the filter. @example { params: { me: "u1" } } */
    params?: Record<string, unknown>;
    /** Sort fields, `-` for descending. @example { sort: "-created,title" } */
    sort?: string;
    /** Page number (1-based). @example { page: 2 } */
    page?: number;
    /** Items per page. @example { perPage: 50 } */
    perPage?: number;
    /** Cursor from a previous page (keyset pagination). @example { cursor: prev.next_cursor } */
    cursor?: string;
  }

  /** A page of records. */
  export interface ListResult<T = Record<string, unknown>> {
    /** The records. @example for (const t of page.items) console.log(t.title); */
    items: T[];
    page?: number;
    per_page?: number;
    total_items?: number;
    total_pages?: number;
    next_cursor?: string | null;
    [key: string]: unknown;
  }

  /** A record: its fields plus `id` and `version`. */
  export type RecordData = Record<string, unknown> & { id?: string; version?: number };

  /** One collection, as the caller (collection rules, field rules, ACLs and tenancy apply). */
  export interface CollectionHandle {
    /** One record by id (needs `db.read`). @example const todo = await ctx.db.from("todos").get(id); */
    get(id: string): Promise<RecordData>;
    /** A page of records (needs `db.read`). @example const { items } = await ctx.db.from("todos").list({ filter: "done = false" }); */
    list(options?: ListOptions): Promise<ListResult<RecordData>>;
    /** Creates a record (needs `db.write`). @example const t = await ctx.db.from("todos").create({ title: "Buy milk" }); */
    create(data: Record<string, unknown>): Promise<RecordData>;
    /**
     * Updates fields of a record (needs `db.write`); with `expectedVersion` it fails on a concurrent change.
     * @example await ctx.db.from("todos").update(t.id, { done: true }, { expectedVersion: t.version });
     */
    update(id: string, data: Record<string, unknown>, options?: { expectedVersion?: number }): Promise<RecordData>;
    /** Deletes a record (needs `db.write`). @example await ctx.db.from("todos").delete(id); */
    delete(id: string): Promise<void>;
  }

  /** Records. */
  export interface Db {
    /** A collection by name. @example const todos = ctx.db.from("todos"); */
    from(collection: string): CollectionHandle;
  }

  /** A vector query. */
  export interface VectorSearch {
    /** Query vector. @example { vector: embedding, k: 5 } */
    vector?: number[];
    /** Keyword query; with `vector` too, a hybrid search. @example { text: "red shoes", k: 10 } */
    text?: string;
    /** Number of results (default 10). @example { k: 3 } */
    k?: number;
    /** Metadata filter. @example { filter: "category = 'news'" } */
    filter?: string;
    /** Values for `{:name}` parameters of the filter. @example { filter: "year >= {:y}", params: { y: 2024 } } */
    params?: Record<string, unknown>;
    /** Vector field, when the collection has several. @example { field: "title_embedding" } */
    field?: string;
    /** Return each hit's vector. @example { includeVectors: true } */
    includeVectors?: boolean;
    /** Return each hit's text. @example { includeText: true } */
    includeText?: boolean;
    /** Brute force instead of the index (ground truth, small sets). @example { vector: q, exact: true } */
    exact?: boolean;
  }

  /** An item to store in a vector collection. */
  export interface VectorItem {
    /** Id (generated when absent). @example { id: "doc-1", vector: [0.1, 0.2] } */
    id?: string;
    /** The vector of a collection with one vector field. @example { vector: [0.1, 0.2, 0.3] } */
    vector?: number[];
    /** Vectors by field name. @example { vectors: { title: [0.1], body: [0.2] } } */
    vectors?: Record<string, number[]>;
    /** Filterable metadata. @example { metadata: { category: "news" } } */
    metadata?: Record<string, unknown>;
    /** Text for keyword search. @example { text: "Breaking news" } */
    text?: string;
  }

  /** One vector collection, as the caller (needs `engine:vector_read` / `engine:vector_write`). */
  export interface VectorHandle {
    /** Nearest neighbours (needs `vector.read`). @example const { hits } = await ctx.vector.from("docs").search({ vector: q, k: 5 }); */
    search(query: VectorSearch): Promise<{ hits: Array<{ id: string; score: number; metadata?: Record<string, unknown>; [key: string]: unknown }>; [key: string]: unknown }>;
    /** One item (needs `vector.read`). @example const item = await ctx.vector.from("docs").get("doc-1"); */
    get(id: string): Promise<Record<string, unknown>>;
    /** Adds or replaces items (needs `vector.write`). @example await ctx.vector.from("docs").upsert({ id: "doc-1", vector: v }); */
    upsert(items: VectorItem | VectorItem[]): Promise<Record<string, unknown>>;
    /** Deletes items (needs `vector.write`). @example await ctx.vector.from("docs").delete(["doc-1", "doc-2"]); */
    delete(ids: string | string[]): Promise<{ deleted: number }>;
  }

  /** Vector search. */
  export interface Vector {
    /** A vector collection by name. @example const docs = ctx.vector.from("docs"); */
    from(collection: string): VectorHandle;
  }

  /** Redis-style key-value commands. Values are strings (numbers and objects are converted). */
  export interface Kv {
    /** A value, or null. @example const v = await ctx.kv.get("greeting"); */
    get(key: string): Promise<string | null>;
    /** Sets a value; `ttlMs` expires it. @example await ctx.kv.set("session", token, { ttlMs: 60_000 }); */
    set(key: string, value: unknown, options?: { ttlMs?: number; nx?: boolean; xx?: boolean }): Promise<boolean>;
    /** Deletes keys; returns how many existed. @example await ctx.kv.del("a", "b"); */
    del(...keys: string[]): Promise<number>;
    /** Adds `by` (default 1) and returns the new value. @example const hits = await ctx.kv.incr("hits"); */
    incr(key: string, by?: number): Promise<number>;
    /** Subtracts `by` (default 1). @example await ctx.kv.decr("stock:sku-1"); */
    decr(key: string, by?: number): Promise<number>;
    /** Sets a time to live in milliseconds. @example await ctx.kv.expire("session", 30_000); */
    expire(key: string, ttlMs: number): Promise<boolean>;
    /** Milliseconds left, -1 without expiry, -2 when missing. @example const left = await ctx.kv.ttl("session"); */
    ttl(key: string): Promise<number>;
    /** Whether a key exists. @example if (await ctx.kv.exists("lock")) return new Response("busy", { status: 409 }); */
    exists(key: string): Promise<boolean>;
    /** Keys starting with `prefix`, sorted. @example const users = await ctx.kv.keys("user:"); */
    keys(prefix?: string): Promise<string[]>;
    /** A field of a hash. @example const name = await ctx.kv.hget("user:1", "name"); */
    hget(key: string, field: string): Promise<string | null>;
    /** Sets fields of a hash. @example await ctx.kv.hset("user:1", { name: "Ada", visits: 1 }); */
    hset(key: string, field: string, value: unknown): Promise<number>;
    hset(key: string, fields: Record<string, unknown>): Promise<number>;
    /** Deletes fields of a hash. @example await ctx.kv.hdel("user:1", "visits"); */
    hdel(key: string, ...fields: string[]): Promise<number>;
    /** Every field of a hash. @example const user = await ctx.kv.hgetall("user:1"); */
    hgetall(key: string): Promise<Record<string, string>>;
    /** Any other command. @example await ctx.kv.command("LPUSH", "queue", "job-1"); */
    command(...args: Array<string | number>): Promise<unknown>;
  }

  /** The function's private key-value namespace (`fn.<name>`), plus access to application namespaces. */
  export interface KvRoot extends Kv {
    /**
     * An application namespace. Needs a key-value rule for this function
     * (an administrator sets it: PUT /api/v1/functions/{name}/kv-rules) and
     * the caller's own key-value permissions.
     * @example const cache = ctx.kv.namespace("cache"); await cache.set("k", "v");
     */
    namespace(name: string): Kv;
  }

  /** An object read from storage. */
  export interface StoredObject {
    /** The key. @example obj.key */
    key: string;
    /** Size in bytes. @example obj.size */
    size: number;
    /** Content type. @example new Response(await obj.bytes(), { headers: { "content-type": obj.contentType } }) */
    contentType: string;
    /** The bytes. @example const bytes = await obj.bytes(); */
    bytes(): Promise<Uint8Array>;
    /** The bytes as an ArrayBuffer. @example const buf = await obj.arrayBuffer(); */
    arrayBuffer(): Promise<ArrayBuffer>;
    /** The bytes as UTF-8 text. @example const text = await obj.text(); */
    text(): Promise<string>;
    /** The bytes as JSON. @example const cfg = await obj.json<{ theme: string }>(); */
    json<T = unknown>(): Promise<T>;
    [key: string]: unknown;
  }

  /** One storage bucket, as the caller (bucket ACLs and policies apply). */
  export interface BucketHandle {
    /** The object, or null when it does not exist (needs `storage.read`; at most 4 MiB, use signedUrl beyond). @example const obj = await ctx.storage.from("avatars").get("u1.png"); */
    get(key: string): Promise<StoredObject | null>;
    /** Stores an object (needs `storage.write`; at most 4 MiB). @example await ctx.storage.from("reports").put("2026/11.json", JSON.stringify(data), { contentType: "application/json" }); */
    put(
      key: string,
      body: string | ArrayBuffer | ArrayBufferView | Blob | URLSearchParams,
      options?: { contentType?: string; metadata?: Record<string, string> },
    ): Promise<Record<string, unknown>>;
    /** Lists objects under a prefix (needs `storage.read`). @example const page = await ctx.storage.from("reports").list("2026/", { limit: 100 }); */
    list(prefix?: string, options?: { limit?: number; cursor?: string; delimiter?: string }): Promise<Record<string, unknown>>;
    /** Deletes an object (needs `storage.write`). @example await ctx.storage.from("tmp").delete("upload.bin"); */
    delete(key: string): Promise<Record<string, unknown>>;
    /** A time-limited download (GET, `storage.read`) or upload (PUT, `storage.write`) URL. @example const { url } = await ctx.storage.from("videos").signedUrl("intro.mp4", { expiresIn: 300 }); */
    signedUrl(
      key: string,
      options?: { expiresIn?: number; method?: "GET" | "PUT"; downloadAs?: string; contentType?: string },
    ): Promise<{ url: string; method: "GET" | "PUT"; expires_at: string }>;
  }

  /** Object storage. */
  export interface Storage {
    /** A bucket by name. @example const avatars = ctx.storage.from("avatars"); */
    from(bucket: string): BucketHandle;
  }

  /** Topics and queues. */
  export interface Queues {
    /** Publishes a JSON message on a topic (needs `queues.publish`). @example await ctx.queues.publish("orders", { id: order.id }, { key: order.customer }); */
    publish(
      topic: string,
      message: unknown,
      options?: { key?: string; attributes?: Record<string, string>; dedupId?: string },
    ): Promise<{ message_id: string; seq: number; duplicate: boolean }>;
  }

  /** Background jobs. */
  export interface Jobs {
    /**
     * Enqueues a job (needs `jobs.enqueue`); kind `function.invoke` with `{ function, input }` runs a function in the background.
     * @example await ctx.jobs.enqueue("function.invoke", { function: "send-email", input: { to } }, { delayMs: 5000 });
     */
    enqueue(
      kind: string,
      payload: unknown,
      options?: {
        runAt?: string | Date;
        delayMs?: number;
        maxAttempts?: number;
        queue?: string;
        priority?: number;
        idempotencyKey?: string;
        timeoutMs?: number;
      },
    ): Promise<{ job: Record<string, unknown>; created: boolean }>;
  }

  /** Realtime channels. */
  export interface Realtime {
    /** Publishes `{event, payload}` on a channel (needs `realtime.publish`). @example await ctx.realtime.broadcast("room:42", "message", { text: "hi" }); */
    broadcast(channel: string, event: string, payload: unknown): Promise<void>;
  }

  /** The engines a function reaches. */
  export interface Services {
    /** Records. @example await ctx.db.from("todos").list(); */
    readonly db: Db;
    /** Vector search. @example await ctx.vector.from("docs").search({ vector: q }); */
    readonly vector: Vector;
    /** Key-value data (`kv` capability). @example await ctx.kv.incr("hits"); */
    readonly kv: KvRoot;
    /** Object storage. @example await ctx.storage.from("avatars").get("u1.png"); */
    readonly storage: Storage;
    /** Topics and queues. @example await ctx.queues.publish("orders", order); */
    readonly queues: Queues;
    /** Background jobs. @example await ctx.jobs.enqueue("function.invoke", { function: "report" }); */
    readonly jobs: Jobs;
    /** Realtime channels. @example await ctx.realtime.broadcast("news", "posted", post); */
    readonly realtime: Realtime;
  }

  /** The second argument of the handler. Services run as the caller, so ForgeBase's rules apply. */
  export interface Context extends Services {
    /**
     * The caller, or null for an anonymous request. Hooks see the writer; jobs and after-hooks the function itself.
     * @example if (!ctx.auth) return new Response("sign in first", { status: 401 });
     */
    readonly auth: AuthInfo | null;
    /**
     * Verifies a bearer token, also in anonymous requests (webhooks); null when invalid.
     * @example const who = await ctx.verifyToken(req.headers.get("x-token") ?? "");
     */
    verifyToken(token: string): Promise<AuthInfo | null>;
    /**
     * Plain environment values first, then secrets (`env`, `secrets` capabilities).
     * @example const key = ctx.env.get("STRIPE_KEY");
     */
    readonly env: {
      /** A value, or undefined. @example ctx.env.get("API_URL") ?? "https://api.example.com" */
      get(name: string): string | undefined;
    };
    /**
     * The same services as the function's privileged principal: row rules, field rules and ACLs do not apply.
     * Needs the `service_role` capability, which only an administrator can grant.
     * @example const all = await ctx.admin.db.from("todos").list();
     */
    readonly admin: Services;
    /** `http`, `hook` or `job`. @example if (ctx.trigger === "hook") { ... } */
    readonly trigger: string;
    /** The hook or job input; null for HTTP requests. @example const ev = ctx.event as { collection: string; record: { id: string } }; */
    readonly event: Record<string, unknown> | null;
    /** Id of the request (also the `x-request-id` header and the execution log entry). @example console.log("request", ctx.requestId); */
    readonly requestId: string | null;
    /** This function and the version that runs. @example console.log(ctx.function.name, ctx.function.version); */
    readonly function: { readonly name: string; readonly version: number };
    /** Logs a rejection of `promise` instead of failing the request. @example ctx.waitUntil(ctx.kv.incr("visits")); */
    waitUntil(promise: Promise<unknown>): void;
  }

  /** A request handler. */
  export type Handler = (req: Request, ctx: Context) => Response | unknown | Promise<Response | unknown>;

  /**
   * Registers the function's handler (call it once). For HTTP requests return a Response, or any JSON value
   * (sent as JSON); a before-hook returns `{ allow: false, reason }` to reject a write; a job returns its result.
   * @example
   * serve(async (req, ctx) => Response.json({ hello: ctx.auth?.id ?? "world" }));
   */
  export function serve(handler: Handler | { fetch: Handler }): void;

  /** The module object. @example import forgebase from "forgebase"; forgebase.serve(() => new Response("ok")); */
  const forgebase: { serve: typeof serve; ForgeBaseError: typeof ForgeBaseError };
  export default forgebase;
}

/**
 * The handler registration, also available without the import.
 * @example serve(() => new Response("ok"));
 */
declare function serve(handler: import("forgebase").Handler | { fetch: import("forgebase").Handler }): void;

/** Deno-compatible helpers. */
declare namespace Deno {
  /** Environment: plain values first, then secrets. @example const url = Deno.env.get("API_URL"); */
  const env: {
    /** A value, or undefined. @example Deno.env.get("API_URL") */
    get(name: string): string | undefined;
  };
  /** Same as `serve` from "forgebase". @example Deno.serve((req) => new Response("ok")); */
  function serve(handler: import("forgebase").Handler | { fetch: import("forgebase").Handler }): void;
}
