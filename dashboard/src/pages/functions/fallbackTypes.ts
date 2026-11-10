/** Bundled copy of the `forgebase` module declarations (the contract's function API), used when `GET /api/v1/functions/types` fails. */
export const FALLBACK_FORGEBASE_DTS = `declare module "forgebase" {
  export interface Auth { id: string; type: string; roles: string[]; permissions: string[]; realm?: string; verify(token: string): Promise<Auth | null> }
  export interface ListOptions { filter?: string; sort?: string; page?: number; perPage?: number; cursor?: string }
  export type Row = Record<string, unknown> & { id: string };
  export interface Records {
    get(id: string): Promise<Row | null>;
    list(opts?: ListOptions): Promise<{ items: Row[]; nextCursor?: string }>;
    create(data: Record<string, unknown>): Promise<Row>;
    update(id: string, data: Record<string, unknown>, opts?: { expectedVersion?: number }): Promise<Row>;
    delete(id: string): Promise<void>;
  }
  export interface Db { from(collection: string): Records }
  export interface Vector {
    from(collection: string): {
      search(q: { vector?: number[]; text?: string; k?: number; filter?: string }): Promise<Row[]>;
      upsert(rows: Record<string, unknown>[]): Promise<void>;
      delete(id: string): Promise<void>;
    };
  }
  export interface Kv {
    get(key: string): Promise<string | null>;
    set(key: string, value: string, opts?: { ttlMs?: number }): Promise<void>;
    del(key: string): Promise<void>;
    incr(key: string, by?: number): Promise<number>;
    expire(key: string, ttlMs: number): Promise<boolean>;
    exists(key: string): Promise<boolean>;
    keys(prefix?: string): Promise<string[]>;
    hget(key: string, field: string): Promise<string | null>;
    hset(key: string, field: string, value: string): Promise<void>;
    hdel(key: string, field: string): Promise<void>;
    hgetall(key: string): Promise<Record<string, string>>;
  }
  export interface Storage {
    from(bucket: string): {
      get(key: string): Promise<Uint8Array | null>;
      put(key: string, bytes: Uint8Array | string, opts?: { contentType?: string }): Promise<void>;
      list(prefix?: string): Promise<string[]>;
      delete(key: string): Promise<void>;
      signedUrl(key: string, opts?: { expiresIn?: number }): Promise<string>;
    };
  }
  export interface Admin { db: Db; vector: Vector; kv: Kv; storage: Storage }
  export interface Context {
    auth: Auth | null;
    env: { get(name: string): string | undefined };
    db: Db;
    vector: Vector;
    kv: Kv;
    storage: Storage;
    queues: { publish(topic: string, message: unknown, opts?: { key?: string }): Promise<void> };
    jobs: { enqueue(kind: string, payload: unknown, opts?: { runAt?: string; maxAttempts?: number }): Promise<string> };
    realtime: { broadcast(channel: string, event: string, payload: unknown): Promise<void> };
    /** Present only when the version holds the service_role capability. */
    admin?: Admin;
  }
  export type Handler = (req: Request, ctx: Context) => Response | Promise<Response>;
  export function serve(handler: Handler): void;
}
declare const Deno: { env: { get(name: string): string | undefined } };
`;
