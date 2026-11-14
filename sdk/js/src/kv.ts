import { ApiError, NetworkError, RealtimeError } from "./errors";
import type { Transport } from "./http";
import { sseEvents } from "./queues";
import type { Schemas } from "./types";

/*
 * Key-value engine (`/api/v1/kv`): `client.kv`.
 * Strings, hashes, lists, sets, sorted sets, streams, TTLs, generic commands and pub/sub, all inside a namespace:
 * `const cache = fb.kv.ns("cache"); await cache.set("k", "v", { ttlMs: 60_000 })`.
 */

/** A value read from the engine: UTF-8 text, or `{ base64 }` for bytes that are not valid UTF-8. */
export type KvBin = string | { base64: string };
/** A value to write: text, raw bytes (sent as base64), or an already encoded `{ base64 }`. */
export type KvValue = string | Uint8Array | { base64: string };

/** One reply of a generic command (`exec` / `batch`): `null`, a status or integer, a value, or an array of those. */
export type KvReply = null | string | number | { base64: string } | KvReply[];

export type KvStats = Schemas["KvStats"];
export type KvNamespaceInfo = Schemas["NamespaceInfo"];

export type KvType = "string" | "hash" | "list" | "set" | "zset" | "stream";

export interface KvStreamEntry {
  /** `<ms>-<seq>`. */
  id: string;
  fields: Record<string, KvBin>;
}

interface KvKeyBase {
  key: string;
  /** Milliseconds until the key expires; `null` when it does not expire. */
  ttl_ms: number | null;
  /** Length: bytes, fields, items, members or entries. */
  size: number;
  /** `value` was cut to the `limit` of the call. */
  truncated: boolean;
}

/** `GET .../keys/{key}`: type, TTL, size and the value, shaped by `type`. */
export type KvKey =
  | (KvKeyBase & { type: "string"; value: KvBin })
  | (KvKeyBase & { type: "hash"; value: Record<string, KvBin> })
  | (KvKeyBase & { type: "list" | "set"; value: KvBin[] })
  | (KvKeyBase & { type: "zset"; value: { member: KvBin; score: number | null }[] })
  | (KvKeyBase & { type: "stream"; value: KvStreamEntry[] });

export interface KvScanOptions {
  /** Cursor of the previous page (`"0"` or absent starts). */
  cursor?: string;
  /** Glob pattern (`user:*`). */
  match?: string;
  /** Keys per page (1 to 1000). */
  count?: number;
  /** Only keys of this type. */
  type?: KvType;
}

export interface KvScanPage {
  /** Pass back as `cursor` for the next page; `"0"` when the scan is done. */
  cursor: string;
  keys: string[];
}

export interface KvSetOptions {
  /** Expire after this many milliseconds. */
  ttlMs?: number;
  /** Only set when the key does not exist. */
  nx?: boolean;
  /** Only set when the key exists. */
  xx?: boolean;
  /** Keep the key's current TTL. */
  keepTtl?: boolean;
}

export type KvListSide = "left" | "right";

export interface KvStreamRangeOptions {
  /** First id (default `-`, the start). */
  start?: string;
  /** Last id (default `+`, the end). */
  end?: string;
  /** Most entries (default 100). */
  count?: number;
  /** Newest first. */
  rev?: boolean;
}

export interface KvStreamReadOptions {
  /** Entries after this id (default `0-0`; `$` = only newer than now). */
  after?: string;
  /** Most entries (default 100). */
  count?: number;
}

export interface KvStreamAddOptions {
  /** Entry id (`ms-seq`); generated when absent. */
  id?: string;
  /** Keep at most this many entries. */
  maxLen?: number;
}

/** One result of `batch`: `{ result }`, or `{ error }` for a command that failed (the rest still ran). */
export type KvBatchResult = { result: KvReply } | { error: { code: string; message: string } };

/** A command argument of `exec` / `batch`: numbers are sent in decimal. */
export type KvArg = KvValue | number | bigint;

export interface KvChannel {
  name: string;
  subscribers: number;
}

export interface KvMessage {
  channel: string;
  message: KvBin;
}

/** A stretch in which `subscribe` may have missed messages. */
export interface KvGap {
  /** `reconnect`: the stream dropped and was re-established; `lagged`: the server dropped messages for a slow reader. */
  reason: "reconnect" | "lagged";
  /** `lagged`: how many messages were dropped. Unknown (`undefined`) for a reconnect. */
  missed?: number;
  /** `reconnect`: when the old stream ended. */
  from?: Date;
  /** `reconnect`: when the new stream opened. Messages published between `from` and `to` were not delivered. */
  to?: Date;
}

export interface KvSubscribeOptions {
  /**
   * Reconnect after a dropped stream (default true). KV pub/sub is fire-and-forget: the engine stores nothing and offers no
   * cursor, so **messages published while the stream is down are lost** and cannot be replayed. Pass `onGap` to find out when
   * that may have happened (and re-read the state you mirror), or use `fb.queues` / `fb.realtime` where delivery matters.
   */
  reconnect?: boolean;
  /** Called after a reconnect (with the span that may have lost messages) and when the server reports a slow reader missed some. */
  onGap?: (gap: KvGap) => void;
  /** Called with errors that do not end the subscription (a handler that threw, a dropped stream). */
  onError?: (err: unknown) => void;
  /** Called when the server says a slow reader missed `n` messages. */
  onLagged?: (missed: number) => void;
  signal?: AbortSignal;
}

export interface KvSubscription {
  /** Stops the subscription. */
  unsubscribe(): void;
  /** Settles when the stream ends: resolves after `unsubscribe`, rejects when the server refuses it for good. */
  done: Promise<void>;
}

const enc = encodeURIComponent;
const K = "/api/v1/kv";

const compact = <T extends Record<string, unknown>>(o: T): T => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;

/** Standard base64 of bytes (browser and Node). */
function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

/** The bytes of a value read from the engine (text is UTF-8 encoded). */
export function kvBytes(v: KvBin): Uint8Array {
  if (typeof v === "string") return new TextEncoder().encode(v);
  const bin = atob(v.base64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** The text of a value read from the engine (bytes are decoded as UTF-8, lossy). */
export function kvText(v: KvBin): string {
  return typeof v === "string" ? v : new TextDecoder().decode(kvBytes(v));
}

const wireValue = (v: KvValue | KvArg): string | { base64: string } => {
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "bigint") return String(v);
  if (v instanceof Uint8Array) return { base64: toBase64(v) };
  return v;
};

const wireFields = (fields: Record<string, KvValue>): Record<string, string | { base64: string }> =>
  Object.fromEntries(Object.entries(fields).map(([k, v]) => [k, wireValue(v)]));

/** Namespaces, engine statistics and generic access: `client.kv`. Key operations live on a namespace handle: `client.kv.ns("cache")`. */
export class KvApi {
  constructor(
    private readonly t: Transport,
    private readonly sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
  ) {}

  /** A handle on one namespace; every key operation goes through it. Cheap: create as many as you like. */
  ns(name: string): KvNamespace {
    return new KvNamespace(this.t, name, this.sleep);
  }

  /** Engine statistics: memory, hit rate, evictions, unsaved keys (administrators). */
  info(): Promise<KvStats> {
    return this.t.request(`${K}/_info`);
  }

  /** Namespaces of the caller's tenant that hold keys. */
  async namespaces(): Promise<KvNamespaceInfo[]> {
    return (await this.t.request<{ namespaces: KvNamespaceInfo[] }>(`${K}/_namespaces`)).namespaces;
  }

  /** Deletes every key of a namespace; resolves with the number of keys removed. */
  async flush(namespace: string): Promise<number> {
    return (await this.t.request<{ deleted: number }>(`${K}/${enc(namespace)}`, { method: "DELETE" })).deleted;
  }
}

/** The key operations of one namespace. */
export class KvNamespace {
  constructor(
    private readonly t: Transport,
    /** The namespace name. */
    readonly name: string,
    private readonly sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
  ) {}

  private path(rest = ""): string {
    return `${K}/${enc(this.name)}${rest}`;
  }

  private keyPath(kind: string, key: string, rest = ""): string {
    return this.path(`/${kind}/${enc(key)}${rest}`);
  }

  // ------------------------------------------------------------------ keys

  /** One page of keys (`SCAN`): by key order, tolerant of concurrent changes. */
  scan(o: KvScanOptions = {}): Promise<KvScanPage> {
    return this.t.request(this.path("/keys"), { query: { cursor: o.cursor, match: o.match, count: o.count, type: o.type } });
  }

  /** Every key matching the options, page after page. */
  async *keys(o: Omit<KvScanOptions, "cursor"> = {}): AsyncGenerator<string> {
    let cursor: string | undefined;
    do {
      const page: KvScanPage = await this.scan({ ...o, cursor });
      yield* page.keys;
      cursor = page.cursor;
    } while (cursor && cursor !== "0");
  }

  /** Type, TTL, size and value of a key; at most `limit` items of a collection (default 1000, up to 10 000). A missing key is a `NotFoundError`. */
  get(key: string, o: { limit?: number } = {}): Promise<KvKey> {
    return this.t.request(this.path(`/keys/${enc(key)}`), { query: { limit: o.limit } });
  }

  /** Deletes a key of any type; `false` when there was none. */
  async delete(key: string): Promise<boolean> {
    return (await this.t.request<{ deleted: boolean }>(this.path(`/keys/${enc(key)}`), { method: "DELETE" })).deleted;
  }

  /** Makes the key expire in `ttlMs` milliseconds. A missing key is a `NotFoundError`. */
  async expire(key: string, ttlMs: number): Promise<void> {
    await this.t.request(this.path(`/keys/${enc(key)}/ttl`), { method: "PUT", body: { ttl_ms: ttlMs } });
  }

  /** Removes the TTL; `false` when the key had none. A missing key is a `NotFoundError`. */
  async persist(key: string): Promise<boolean> {
    return (await this.t.request<{ persisted: boolean }>(this.path(`/keys/${enc(key)}/ttl`), { method: "DELETE" })).persisted;
  }

  // --------------------------------------------------------------- strings

  /** Sets a string. Resolves `false` when `nx` / `xx` prevented the write. */
  async set(key: string, value: KvValue, o: KvSetOptions = {}): Promise<boolean> {
    const body = compact({ value: wireValue(value), ttl_ms: o.ttlMs, nx: o.nx, xx: o.xx, keepttl: o.keepTtl });
    return (await this.t.request<{ stored: boolean }>(this.keyPath("strings", key), { method: "PUT", body })).stored;
  }

  /** The string at `key` as text, or `null` when there is none (or the key holds another type). */
  async text(key: string): Promise<string | null> {
    const [v] = await this.mget([key]);
    return v === null || v === undefined ? null : kvText(v);
  }

  /** Adds `by` (default 1; negative decrements) to the integer in a string; resolves with the new value. */
  async incr(key: string, by?: number): Promise<number> {
    return (await this.t.request<{ value: number }>(this.keyPath("strings", key, "/incr"), { method: "POST", body: compact({ by }) })).value;
  }

  /** Reads several strings at once, in key order; `null` for a missing key or one of another type. */
  async mget(keys: string[]): Promise<(KvBin | null)[]> {
    return (await this.t.request<{ values: (KvBin | null)[] }>(this.path("/mget"), { method: "POST", body: { keys } })).values;
  }

  /** Writes several strings in one step; resolves with how many. */
  async mset(entries: Record<string, KvValue> | [string, KvValue][]): Promise<number> {
    const list = Array.isArray(entries) ? entries : Object.entries(entries);
    const body = { entries: list.map(([key, value]) => ({ key, value: wireValue(value) })) };
    return (await this.t.request<{ stored: number }>(this.path("/mset"), { method: "POST", body })).stored;
  }

  // ---------------------------------------------------------------- hashes

  /** Sets hash fields; resolves with the number of new fields. */
  async hashSet(key: string, fields: Record<string, KvValue>): Promise<number> {
    return (await this.t.request<{ added: number }>(this.keyPath("hashes", key, "/set"), { method: "POST", body: { fields: wireFields(fields) } })).added;
  }

  /** Removes hash fields; resolves with how many existed. */
  async hashDelete(key: string, fields: string[]): Promise<number> {
    return (await this.t.request<{ removed: number }>(this.keyPath("hashes", key, "/delete"), { method: "POST", body: { fields } })).removed;
  }

  /** Adds `by` (default 1) to the integer in a hash field; resolves with the new value. */
  async hashIncr(key: string, field: string, by?: number): Promise<number> {
    return (await this.t.request<{ value: number }>(this.keyPath("hashes", key, "/incr"), { method: "POST", body: compact({ field, by }) })).value;
  }

  // ----------------------------------------------------------------- lists

  /** Pushes values onto the head (`left`) or tail (`right`), one after the other; resolves with the new length. */
  async listPush(key: string, side: KvListSide, values: KvValue[]): Promise<number> {
    return (await this.t.request<{ length: number }>(this.keyPath("lists", key, "/push"), { method: "POST", body: { side, values: values.map(wireValue) } })).length;
  }

  /** Pops `count` (default 1) values from one end; an empty list gives `[]`. */
  async listPop(key: string, side: KvListSide, count?: number): Promise<KvBin[]> {
    return (await this.t.request<{ values: KvBin[] }>(this.keyPath("lists", key, "/pop"), { method: "POST", body: compact({ side, count }) })).values;
  }

  /** Removes occurrences of `value` (`count` 0 or absent: all; `n > 0`: the first `n` from the head; `n < 0`: from the tail); resolves with how many. */
  async listRemove(key: string, value: KvValue, count?: number): Promise<number> {
    return (await this.t.request<{ removed: number }>(this.keyPath("lists", key, "/remove"), { method: "POST", body: compact({ value: wireValue(value), count }) })).removed;
  }

  /** Replaces the element at `index` (negative counts from the tail). Out of range is a `ValidationError`. */
  async listSet(key: string, index: number, value: KvValue): Promise<void> {
    await this.t.request(this.keyPath("lists", key, "/set"), { method: "POST", body: { index, value: wireValue(value) } });
  }

  // ------------------------------------------------------------------ sets

  /** Adds members; resolves with the number of new ones. */
  async setAdd(key: string, members: KvValue[]): Promise<number> {
    return (await this.t.request<{ added: number }>(this.keyPath("sets", key, "/add"), { method: "POST", body: { members: members.map(wireValue) } })).added;
  }

  /** Removes members; resolves with how many were present. */
  async setRemove(key: string, members: KvValue[]): Promise<number> {
    return (await this.t.request<{ removed: number }>(this.keyPath("sets", key, "/remove"), { method: "POST", body: { members: members.map(wireValue) } })).removed;
  }

  // ----------------------------------------------------------- sorted sets

  /** Adds members or re-scores them (scores must be finite; use `exec` for infinities); resolves with the number of new members. */
  async zsetAdd(key: string, members: { member: KvValue; score: number }[]): Promise<number> {
    const body = { members: members.map((m) => ({ member: wireValue(m.member), score: m.score })) };
    return (await this.t.request<{ added: number }>(this.keyPath("zsets", key, "/add"), { method: "POST", body })).added;
  }

  /** Removes members; resolves with how many were present. */
  async zsetRemove(key: string, members: KvValue[]): Promise<number> {
    return (await this.t.request<{ removed: number }>(this.keyPath("zsets", key, "/remove"), { method: "POST", body: { members: members.map(wireValue) } })).removed;
  }

  // --------------------------------------------------------------- streams

  /** Appends an entry; resolves with its id. */
  async streamAdd(key: string, fields: Record<string, KvValue>, o: KvStreamAddOptions = {}): Promise<string> {
    const body = compact({ id: o.id, fields: wireFields(fields), maxlen: o.maxLen });
    return (await this.t.request<{ id: string }>(this.keyPath("streams", key, "/add"), { method: "POST", body })).id;
  }

  /** Deletes entries by id; resolves with how many existed. */
  async streamDelete(key: string, ids: string[]): Promise<number> {
    return (await this.t.request<{ removed: number }>(this.keyPath("streams", key, "/delete"), { method: "POST", body: { ids } })).removed;
  }

  /** Entries between two ids (`-` and `+` are the ends). */
  async streamRange(key: string, o: KvStreamRangeOptions = {}): Promise<KvStreamEntry[]> {
    const query = { start: o.start, end: o.end, count: o.count, rev: o.rev };
    return (await this.t.request<{ entries: KvStreamEntry[] }>(this.keyPath("streams", key, "/range"), { query })).entries;
  }

  /** Entries after an id, without waiting for new ones: poll with the last id you saw, or use pub/sub to learn of new entries. */
  async streamRead(key: string, o: KvStreamReadOptions = {}): Promise<KvStreamEntry[]> {
    return (await this.t.request<{ entries: KvStreamEntry[] }>(this.keyPath("streams", key, "/read"), { query: { after: o.after, count: o.count } })).entries;
  }

  // ------------------------------------------------------- generic command

  /** Runs one command given as an array: `exec(["ZRANGE", "board", 0, -1, "WITHSCORES"])`. */
  async exec(command: KvArg[]): Promise<KvReply> {
    return (await this.t.request<{ result: KvReply }>(this.path("/exec"), { method: "POST", body: { command: command.map(wireValue) } })).result;
  }

  /** Runs 1 to 1000 commands in order with no other command in between. A failing command reports `{ error }` and the rest still run; earlier ones are not undone. */
  async batch(commands: KvArg[][]): Promise<KvBatchResult[]> {
    const body = { commands: commands.map((c) => c.map(wireValue)) };
    return (await this.t.request<{ results: KvBatchResult[] }>(this.path("/batch"), { method: "POST", body })).results;
  }

  // --------------------------------------------------------------- pub/sub

  /** Publishes to the subscribers connected now (messages are not stored); resolves with how many received it. */
  async publish(channel: string, message: KvValue): Promise<number> {
    return (await this.t.request<{ receivers: number }>(this.path(`/publish/${enc(channel)}`), { method: "POST", body: { message: wireValue(message) } })).receivers;
  }

  /** Live channels of the namespace (those with at least one subscriber). */
  async channels(): Promise<KvChannel[]> {
    return (await this.t.request<{ channels: KvChannel[] }>(this.path("/channels"))).channels;
  }

  /**
   * Subscribes to a channel (server-sent events): `handler` runs for every message published after the stream opens.
   * A dropped stream reconnects with backoff; a refusal (missing permission) ends it and rejects `done`.
   */
  subscribe(channel: string, handler: (m: KvMessage) => void | Promise<void>, o: KvSubscribeOptions = {}): KvSubscription {
    const ctrl = new AbortController();
    if (o.signal) {
      if (o.signal.aborted) ctrl.abort();
      else o.signal.addEventListener("abort", () => ctrl.abort(), { once: true });
    }
    const reconnect = o.reconnect ?? true;
    const report = (e: unknown) => {
      try {
        o.onError?.(e);
      } catch {
        /* a throwing error callback must not break the stream */
      }
    };
    const run = async () => {
      let delay = 500;
      // When the stream last went down (set once it was up before), to tell `onGap` what may have been missed.
      let lostAt: Date | undefined;
      let everOpened = false;
      while (!ctrl.signal.aborted) {
        let opened = false;
        try {
          const res = await this.t.send(this.path(`/subscribe/${enc(channel)}`), { headers: { Accept: "text/event-stream" }, signal: ctrl.signal, retry: false });
          if (!res.body) throw new NetworkError("the event stream has no body");
          opened = true;
          if (everOpened && lostAt) {
            const gap: KvGap = { reason: "reconnect", from: lostAt, to: new Date() };
            lostAt = undefined;
            try {
              o.onGap?.(gap);
            } catch {
              /* a throwing callback must not break the stream */
            }
          }
          everOpened = true;
          for await (const ev of sseEvents(res.body)) {
            if (ev.event === "lagged") {
              try {
                o.onLagged?.(Number(ev.data));
                o.onGap?.({ reason: "lagged", missed: Number(ev.data) });
              } catch {
                /* ignore */
              }
            } else if (ev.event === "error") {
              throw new RealtimeError("stream_error", ev.data || "stream error");
            } else if (ev.event === "message" && ev.data) {
              let m: KvMessage;
              try {
                m = JSON.parse(ev.data) as KvMessage;
              } catch {
                continue;
              }
              try {
                await handler(m);
              } catch (e) {
                report(e);
              }
            }
          }
          if (!reconnect) return;
          if (everOpened) lostAt ??= new Date();
        } catch (e) {
          if (ctrl.signal.aborted) return;
          const final = e instanceof RealtimeError || (e instanceof ApiError && e.status >= 400 && e.status < 500 && e.status !== 429 && e.status !== 408);
          if (final || !reconnect) throw e;
          if (everOpened) lostAt ??= new Date();
          report(e);
        }
        if (opened) delay = 500;
        await this.sleep(delay);
        delay = Math.min(delay * 2, 10_000);
      }
    };
    const done = run().catch((e: unknown) => {
      if (ctrl.signal.aborted) return;
      throw e;
    });
    done.catch(() => undefined);
    return { unsubscribe: () => ctrl.abort(), done };
  }
}
