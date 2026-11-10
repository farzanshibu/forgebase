// Type declarations of the ForgeBase function API (ADR-0027).
//
// A function is one ES2023 module that imports only "forgebase" and calls
// serve() once. The server also serves this file at GET /api/v1/functions/types.
// The usual Web globals (Request, Response, Headers, URL, URLSearchParams,
// TextEncoder, TextDecoder, Blob, AbortController, atob, btoa, crypto,
// structuredClone, setTimeout, fetch, console) come from the TypeScript DOM
// library; this file adds what ForgeBase provides on top.

declare module "forgebase" {
  /** A failed host call: `code` is ForgeBase's stable error code (`forbidden`, `capability_denied`, `record_not_found`, ...). */
  export class ForgeBaseError extends Error {
    readonly code: string;
  }

  /** The caller (or, for `ctx.auth.verify`, the token's owner). */
  export interface AuthInfo {
    /** Record, administrator or service-account id. */
    readonly id: string | null;
    /** `user`, `admin` or `service`. */
    readonly type: "user" | "admin" | "service" | "system";
    /** Auth collection of a user. */
    readonly collection: string | null;
    /** RBAC roles for this request. */
    readonly roles: readonly string[];
    /** Granted permission patterns (`data:posts:read`, `engine:kv_read`, `*`). */
    readonly permissions: readonly string[];
    readonly realm: string | null;
    readonly org: string | null;
    readonly tenant: string | null;
    /** Verifies a bearer token (access token, API key or personal access token); `null` when invalid. */
    verify(token: string): Promise<AuthInfo | null>;
  }

  export interface ListOptions {
    /** Filter in ForgeBase's filter language, e.g. `done = false && owner = {:me}`. */
    filter?: string;
    /** Values for `{:name}` parameters of the filter. */
    params?: Record<string, unknown>;
    /** `-created,title`. */
    sort?: string;
    page?: number;
    perPage?: number;
    /** Cursor from a previous page (keyset pagination). */
    cursor?: string;
  }

  export interface ListResult<T = Record<string, unknown>> {
    items: T[];
    page?: number;
    per_page?: number;
    total_items?: number;
    total_pages?: number;
    next_cursor?: string | null;
    [key: string]: unknown;
  }

  export type RecordData = Record<string, unknown> & { id?: string; version?: number };

  export interface CollectionHandle {
    get(id: string): Promise<RecordData>;
    list(options?: ListOptions): Promise<ListResult<RecordData>>;
    create(data: Record<string, unknown>): Promise<RecordData>;
    /** Partial update; with `expectedVersion` it fails on a concurrent change. */
    update(id: string, data: Record<string, unknown>, options?: { expectedVersion?: number }): Promise<RecordData>;
    delete(id: string): Promise<void>;
  }

  export interface Db {
    from(collection: string): CollectionHandle;
  }

  export interface VectorSearch {
    /** Query vector (exact or ANN search). */
    vector?: number[];
    /** Keyword query; with `vector` too, a hybrid search. */
    text?: string;
    k?: number;
    filter?: string;
    params?: Record<string, unknown>;
    /** Vector field, when the collection has several. */
    field?: string;
    includeVectors?: boolean;
    includeText?: boolean;
  }

  export interface VectorItem {
    id?: string;
    /** Shorthand for a collection with one vector field. */
    vector?: number[];
    vectors?: Record<string, number[]>;
    metadata?: Record<string, unknown>;
    text?: string;
  }

  export interface VectorHandle {
    search(query: VectorSearch): Promise<{ hits: Array<{ id: string; score: number; metadata?: Record<string, unknown>; [key: string]: unknown }>; [key: string]: unknown }>;
    get(id: string): Promise<Record<string, unknown>>;
    upsert(items: VectorItem | VectorItem[]): Promise<Record<string, unknown>>;
    delete(ids: string | string[]): Promise<{ deleted: number }>;
  }

  export interface Vector {
    from(collection: string): VectorHandle;
  }

  /** Redis-style key-value commands. Values are strings (numbers and objects are converted). */
  export interface Kv {
    get(key: string): Promise<string | null>;
    set(key: string, value: unknown, options?: { ttlMs?: number; nx?: boolean; xx?: boolean }): Promise<boolean>;
    del(...keys: string[]): Promise<number>;
    incr(key: string, by?: number): Promise<number>;
    decr(key: string, by?: number): Promise<number>;
    /** Sets a time to live in milliseconds. */
    expire(key: string, ttlMs: number): Promise<boolean>;
    /** Milliseconds left, -1 without expiry, -2 when missing. */
    ttl(key: string): Promise<number>;
    exists(key: string): Promise<boolean>;
    /** Keys starting with `prefix`, sorted. */
    keys(prefix?: string): Promise<string[]>;
    hget(key: string, field: string): Promise<string | null>;
    hset(key: string, field: string, value: unknown): Promise<number>;
    hset(key: string, fields: Record<string, unknown>): Promise<number>;
    hdel(key: string, ...fields: string[]): Promise<number>;
    hgetall(key: string): Promise<Record<string, string>>;
    /** Any other command, e.g. `command("LPUSH", "jobs", "a")`. */
    command(...args: Array<string | number>): Promise<unknown>;
  }

  export interface KvRoot extends Kv {
    /**
     * An application namespace. Needs a key-value rule for this function
     * (PUT /api/v1/functions/{name}/kv-rules, administrators) and the
     * caller's own key-value permissions. Without it, `ctx.kv` is the
     * function's private namespace `fn.<name>`.
     */
    namespace(name: string): Kv;
  }

  export interface StoredObject {
    key: string;
    size: number;
    contentType: string;
    bytes(): Promise<Uint8Array>;
    arrayBuffer(): Promise<ArrayBuffer>;
    text(): Promise<string>;
    json<T = unknown>(): Promise<T>;
    [key: string]: unknown;
  }

  export interface BucketHandle {
    /** The object, or `null` when it does not exist (at most 4 MiB; use signedUrl beyond). */
    get(key: string): Promise<StoredObject | null>;
    put(
      key: string,
      body: string | ArrayBuffer | ArrayBufferView | Blob | URLSearchParams,
      options?: { contentType?: string; metadata?: Record<string, string> },
    ): Promise<Record<string, unknown>>;
    list(prefix?: string, options?: { limit?: number; cursor?: string; delimiter?: string }): Promise<Record<string, unknown>>;
    delete(key: string): Promise<Record<string, unknown>>;
    signedUrl(
      key: string,
      options?: { expiresIn?: number; method?: "GET" | "PUT"; downloadAs?: string; contentType?: string },
    ): Promise<{ url: string; method: "GET" | "PUT"; expires_at: string }>;
  }

  export interface Storage {
    from(bucket: string): BucketHandle;
  }

  export interface Queues {
    publish(
      topic: string,
      message: unknown,
      options?: { key?: string; attributes?: Record<string, string>; dedupId?: string },
    ): Promise<{ message_id: string; seq: number; duplicate: boolean }>;
  }

  export interface Jobs {
    /** Enqueues a background job; kind `function.invoke` with `{ function, input }` runs a function. */
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

  export interface Realtime {
    /** Publishes `{event, payload}` on a channel. */
    broadcast(channel: string, event: string, payload: unknown): Promise<void>;
  }

  export interface Services {
    readonly db: Db;
    readonly vector: Vector;
    readonly kv: KvRoot;
    readonly storage: Storage;
    readonly queues: Queues;
    readonly jobs: Jobs;
    readonly realtime: Realtime;
  }

  export interface Context extends Services {
    /** The caller, or `null` for an anonymous request. Hooks see the writer; jobs and after-hooks the function itself. */
    readonly auth: AuthInfo | null;
    /** Verifies a bearer token, also for anonymous requests (webhooks). */
    verifyToken(token: string): Promise<AuthInfo | null>;
    /** Plain environment values first, then secrets (capabilities `env`, `secrets`). */
    readonly env: { get(name: string): string | undefined };
    /**
     * The same services with the function's privileged principal: row
     * rules, field rules and ACLs do not apply. Needs the `service_role`
     * capability, which only an administrator can grant.
     */
    readonly admin: Services;
    /** `http`, `hook`, `job` or `test`. */
    readonly trigger: string;
    /** The hook or job input (`null` for HTTP requests). */
    readonly event: Record<string, unknown> | null;
    readonly requestId: string | null;
    readonly function: { readonly name: string; readonly version: number };
    /** Logs a rejection of `promise` instead of failing; the execution still ends with the response. */
    waitUntil(promise: Promise<unknown>): void;
  }

  export type Handler = (req: Request, ctx: Context) => Response | unknown | Promise<Response | unknown>;

  /**
   * Registers the function's handler. For HTTP requests return a Response
   * (or any JSON value, sent as JSON); for hooks return
   * `{ allow: false, reason }` to reject a write; for jobs, the result.
   */
  export function serve(handler: Handler | { fetch: Handler }): void;

  const forgebase: { serve: typeof serve; ForgeBaseError: typeof ForgeBaseError };
  export default forgebase;
}

declare namespace Deno {
  /** Plain environment values first, then secrets. */
  const env: { get(name: string): string | undefined };
}
