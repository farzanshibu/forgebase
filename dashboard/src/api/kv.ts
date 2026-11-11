/** Typed client for `/api/v1/kv` (Redis-compatible key-value store): keys, per-type edits, command console, pub/sub, stats. */
import { ApiError, request, send } from "./client";
import { authHeaders } from "./token";

const K = "/api/v1/kv";
const enc = encodeURIComponent;

/** A binary-safe value: UTF-8 text, or `{base64}` for bytes that are not UTF-8. */
export type Bin = string | { base64: string };
export type KvType = "string" | "hash" | "list" | "set" | "zset" | "stream";
export const KV_TYPES: KvType[] = ["string", "hash", "list", "set", "zset", "stream"];

export interface KvKey {
  key: string; type: KvType; ttl_ms?: number | null; size: number; truncated: boolean;
  /** string: Bin; hash: {field: Bin}; list/set: Bin[]; zset: {member, score}[]; stream: {id, fields}[]. */
  value: unknown;
}
export interface KvScan { cursor: string; keys: string[] }
export interface KvStats {
  /** `engine` (every tenant) or `tenant` (the caller's tenant only). */
  scope: string; tenant?: string | null;
  keys: number; used_memory_bytes: number; max_memory_bytes: number; eviction_policy: string; hits: number; misses: number;
  expired_keys: number; evicted_keys: number; channels: number; unsaved_keys: number; persistent: boolean;
}
export interface KvNamespace { name: string; keys: number }
export interface StreamEntry { id: Bin; fields: Record<string, Bin> }
export interface ZMember { member: Bin; score: number | null }
export interface KvScanOptions { cursor?: string; match?: string; count?: number; type?: KvType | "" }

export const KV_OPERATIONS = [
  "get /api/v1/kv/_info", "get /api/v1/kv/_namespaces", "delete /api/v1/kv/{ns}", "get /api/v1/kv/{ns}/keys", "get /api/v1/kv/{ns}/keys/{key}",
  "delete /api/v1/kv/{ns}/keys/{key}", "put /api/v1/kv/{ns}/keys/{key}/ttl", "delete /api/v1/kv/{ns}/keys/{key}/ttl", "put /api/v1/kv/{ns}/strings/{key}",
  "post /api/v1/kv/{ns}/hashes/{key}/set", "post /api/v1/kv/{ns}/hashes/{key}/delete", "post /api/v1/kv/{ns}/lists/{key}/push", "post /api/v1/kv/{ns}/lists/{key}/pop", "post /api/v1/kv/{ns}/lists/{key}/remove", "post /api/v1/kv/{ns}/lists/{key}/set",
  "post /api/v1/kv/{ns}/sets/{key}/add", "post /api/v1/kv/{ns}/sets/{key}/remove", "post /api/v1/kv/{ns}/zsets/{key}/add", "post /api/v1/kv/{ns}/zsets/{key}/remove",
  "post /api/v1/kv/{ns}/streams/{key}/add", "post /api/v1/kv/{ns}/streams/{key}/delete", "get /api/v1/kv/{ns}/channels", "get /api/v1/kv/{ns}/streams/{key}/range", "post /api/v1/kv/{ns}/exec", "post /api/v1/kv/{ns}/publish/{channel}", "get /api/v1/kv/{ns}/subscribe/{channel}",
] as const;

/** Display text of a value; non-UTF-8 bytes show as base64 with a marker. */
export function binText(b: Bin | null | undefined): string {
  if (b === null || b === undefined) return "";
  return typeof b === "string" ? b : `base64:${b.base64}`;
}
/** Text typed in an editor back to a wire value (`base64:...` round-trips binary). */
export function binFrom(text: string): Bin {
  return text.startsWith("base64:") ? { base64: text.slice(7) } : text;
}

export const kvInfo = (signal?: AbortSignal) => request<KvStats>(`${K}/_info`, { signal });
export const kvNamespaces = async (signal?: AbortSignal) => (await request<{ namespaces: KvNamespace[] }>(`${K}/_namespaces`, { signal })).namespaces;
export const kvFlush = (ns: string) => send<{ deleted: number }>("DELETE", `${K}/${enc(ns)}`);
export function kvScan(ns: string, o: KvScanOptions = {}, signal?: AbortSignal) {
  const q = new URLSearchParams();
  if (o.cursor) q.set("cursor", o.cursor);
  if (o.match) q.set("match", o.match);
  if (o.count) q.set("count", String(o.count));
  if (o.type) q.set("type", o.type);
  const s = q.toString();
  return request<KvScan>(`${K}/${enc(ns)}/keys${s ? `?${s}` : ""}`, { signal });
}
export const kvGet = (ns: string, key: string, limit?: number, signal?: AbortSignal) =>
  request<KvKey>(`${K}/${enc(ns)}/keys/${enc(key)}${limit ? `?limit=${limit}` : ""}`, { signal });
export const kvDelete = (ns: string, key: string) => send<{ deleted: boolean }>("DELETE", `${K}/${enc(ns)}/keys/${enc(key)}`);
export const kvSetTtl = (ns: string, key: string, ttlMs: number) => send<{ applied: boolean }>("PUT", `${K}/${enc(ns)}/keys/${enc(key)}/ttl`, { ttl_ms: ttlMs });
export const kvClearTtl = (ns: string, key: string) => send<{ persisted: boolean }>("DELETE", `${K}/${enc(ns)}/keys/${enc(key)}/ttl`);
export const kvSetString = (ns: string, key: string, value: Bin, opts: { ttl_ms?: number; nx?: boolean; xx?: boolean; keepttl?: boolean } = {}) =>
  send<{ stored: boolean }>("PUT", `${K}/${enc(ns)}/strings/${enc(key)}`, { value, ...opts });
export const kvHashSet = (ns: string, key: string, fields: Record<string, Bin>) => send<{ added: number }>("POST", `${K}/${enc(ns)}/hashes/${enc(key)}/set`, { fields });
export const kvHashDelete = (ns: string, key: string, fields: string[]) => send<{ removed: number }>("POST", `${K}/${enc(ns)}/hashes/${enc(key)}/delete`, { fields });
export const kvListPush = (ns: string, key: string, side: "left" | "right", values: Bin[]) => send<{ length: number }>("POST", `${K}/${enc(ns)}/lists/${enc(key)}/push`, { side, values });
export const kvListPop = (ns: string, key: string, side: "left" | "right", count = 1) => send<{ values: Bin[] }>("POST", `${K}/${enc(ns)}/lists/${enc(key)}/pop`, { side, count });
/** LREM: `count` 0 removes every match, n > 0 the first n from the head, n < 0 from the tail. */
export const kvListRemove = (ns: string, key: string, value: Bin, count = 0) => send<{ removed: number }>("POST", `${K}/${enc(ns)}/lists/${enc(key)}/remove`, { value, count });
/** LSET: replace the item at `index` (negative counts from the tail). */
export const kvListSet = (ns: string, key: string, index: number, value: Bin) => send<{ ok: boolean }>("POST", `${K}/${enc(ns)}/lists/${enc(key)}/set`, { index, value });
/** XDEL. */
export const kvStreamDelete = (ns: string, key: string, ids: string[]) => send<{ removed: number }>("POST", `${K}/${enc(ns)}/streams/${enc(key)}/delete`, { ids });
export interface KvChannel { name: string; subscribers: number }
/** Channels with at least one live subscriber in the namespace. */
export const kvChannels = async (ns: string, signal?: AbortSignal) => (await request<{ channels: KvChannel[] }>(`${K}/${enc(ns)}/channels`, { signal })).channels;
export const kvSetAdd = (ns: string, key: string, members: Bin[]) => send<{ added: number }>("POST", `${K}/${enc(ns)}/sets/${enc(key)}/add`, { members });
export const kvSetRemove = (ns: string, key: string, members: Bin[]) => send<{ removed: number }>("POST", `${K}/${enc(ns)}/sets/${enc(key)}/remove`, { members });
export const kvZAdd = (ns: string, key: string, members: { member: Bin; score: number }[]) => send<{ added: number }>("POST", `${K}/${enc(ns)}/zsets/${enc(key)}/add`, { members });
export const kvZRemove = (ns: string, key: string, members: Bin[]) => send<{ removed: number }>("POST", `${K}/${enc(ns)}/zsets/${enc(key)}/remove`, { members });
export const kvStreamAdd = (ns: string, key: string, fields: Record<string, Bin>, o: { id?: string; maxlen?: number } = {}) =>
  send<{ id: Bin }>("POST", `${K}/${enc(ns)}/streams/${enc(key)}/add`, { fields, ...o });
export const kvStreamRange = async (ns: string, key: string, o: { start?: string; end?: string; count?: number; rev?: boolean } = {}, signal?: AbortSignal) => {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(o)) if (v !== undefined && v !== false) q.set(k, String(v));
  const s = q.toString();
  return (await request<{ entries: StreamEntry[] }>(`${K}/${enc(ns)}/streams/${enc(key)}/range${s ? `?${s}` : ""}`, { signal })).entries;
};
/** Run any command given as arguments, e.g. `["INCRBY", "hits", "5"]`. */
export const kvExec = async (ns: string, command: string[]) => (await send<{ result: unknown }>("POST", `${K}/${enc(ns)}/exec`, { command })).result;
export const kvPublish = (ns: string, channel: string, message: Bin) => send<{ receivers: number }>("POST", `${K}/${enc(ns)}/publish/${enc(channel)}`, { message });

export interface PubSubMessage { channel: string; message: Bin }
/**
 * Listen to a channel over server-sent events until `signal` aborts. `onLagged(n)` fires when the server dropped `n` messages.
 * Resolves when the stream ends; rejects with `ApiError` when the server refuses it.
 */
export async function kvSubscribe(ns: string, channel: string, onMessage: (m: PubSubMessage) => void, signal: AbortSignal, onLagged?: (n: number) => void): Promise<void> {
  let res: Response;
  try {
    res = await fetch(`${K}/${enc(ns)}/subscribe/${enc(channel)}`, { headers: { Accept: "text/event-stream", ...(await authHeaders()) }, signal, credentials: "same-origin" });
  } catch (err) {
    if (err instanceof DOMException && err.name === "AbortError") return;
    throw new ApiError({ status: 0, code: "network_error", message: err instanceof Error ? err.message : "Network request failed" });
  }
  if (!res.ok || !res.body) {
    let code = "http_error", message = res.statusText || `HTTP ${res.status}`;
    try { const b = (await res.json()) as { error?: { code?: string; message?: string } }; if (b.error?.code) { code = b.error.code; message = b.error.message ?? message; } } catch { /* not JSON */ }
    throw new ApiError({ status: res.status, code, message });
  }
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  const dispatch = (block: string) => {
    let event = "message", data = "";
    for (const line of block.split("\n")) {
      if (line.startsWith("event:")) event = line.slice(6).trim();
      else if (line.startsWith("data:")) data += (data ? "\n" : "") + line.slice(5).replace(/^ /, "");
    }
    if (event === "lagged") onLagged?.(Number(data) || 0);
    else if (event === "message" && data) {
      try { onMessage(JSON.parse(data) as PubSubMessage); } catch { /* malformed event */ }
    }
  };
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true }).replace(/\r\n/g, "\n");
      let i: number;
      while ((i = buf.indexOf("\n\n")) >= 0) { dispatch(buf.slice(0, i)); buf = buf.slice(i + 2); }
    }
  } catch (err) {
    if (!(err instanceof DOMException && err.name === "AbortError")) throw err;
  }
}
