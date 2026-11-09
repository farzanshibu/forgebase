/** In-memory fake of `/api/v1/kv` with a tiny Redis-like core, for component tests. Plain handlers: the OpenAPI fixture has no kv routes yet (FIX-3). */
import type { KvStats } from "../api";
import { envelope, type MockApi } from "./mockApi";

type V = string | Record<string, string> | string[] | { member: string; score: number }[] | { id: string; fields: Record<string, string> }[];
export interface KvItem { type: "string" | "hash" | "list" | "set" | "zset" | "stream"; value: V; ttl_ms: number | null }
export interface KvState {
  ns: Record<string, Record<string, KvItem>>;
  stats: KvStats;
  /** Commands received by /exec. */
  execs: string[][];
  published: { channel: string; message: unknown }[];
  /** SSE frames the next subscribe stream sends, then it stays open until aborted. */
  frames: string[];
}

export function installKv(mock: MockApi): KvState {
  const st: KvState = {
    ns: {
      default: {
        greeting: { type: "string", value: "hello", ttl_ms: null },
        "user:1": { type: "hash", value: { name: "Ann", age: "31" }, ttl_ms: 90_000 },
        "user:2": { type: "hash", value: { name: "Bo" }, ttl_ms: null },
        todo: { type: "list", value: ["a", "b", "c"], ttl_ms: null },
        tags: { type: "set", value: ["red", "blue"], ttl_ms: null },
        board: { type: "zset", value: [{ member: "ann", score: 10 }, { member: "bo", score: 20 }], ttl_ms: null },
        events: { type: "stream", value: [{ id: "1-0", fields: { kind: "login" } }], ttl_ms: null },
      },
      other: { x: { type: "string", value: "1", ttl_ms: null } },
    },
    stats: { keys: 8, used_memory_bytes: 5 * 1024 * 1024, max_memory_bytes: 10 * 1024 * 1024, eviction_policy: "allkeys-lru", hits: 90, misses: 10, expired_keys: 3, evicted_keys: 1, channels: 2, unsaved_keys: 0, persistent: true },
    execs: [], published: [], frames: [],
  };
  const K = "/api/v1/kv";
  const space = (n: string) => (st.ns[n] ??= {});
  const nokey = () => envelope(404, "kv_key_not_found", "no such key");
  const wrong = () => envelope(409, "kv_wrong_type", "wrong type");
  const get = (n: string, k: string, t: KvItem["type"]): KvItem | "wrong" | undefined => { const it = space(n)[k]; return it && it.type !== t ? "wrong" : it; };

  mock.on("GET", `${K}/_info`, () => ({ status: 200, body: st.stats }));
  mock.on("GET", `${K}/_namespaces`, () => ({ status: 200, body: { namespaces: Object.entries(st.ns).filter(([, v]) => Object.keys(v).length).map(([name, v]) => ({ name, keys: Object.keys(v).length })) } }));
  mock.on("DELETE", `${K}/{ns}`, ({ params }) => { const n = Object.keys(space(params.ns!)).length; st.ns[params.ns!] = {}; return { status: 200, body: { deleted: n } }; });
  mock.on("GET", `${K}/{ns}/keys`, ({ params, query }) => {
    const m = query.get("match"), t = query.get("type"), count = Number(query.get("count") ?? 10), start = Number(query.get("cursor") ?? 0);
    const re = m ? new RegExp(`^${m.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`) : null;
    const all = Object.entries(space(params.ns!)).filter(([k, v]) => (!re || re.test(k)) && (!t || v.type === t)).map(([k]) => k).sort();
    const page = all.slice(start, start + count);
    return { status: 200, body: { cursor: start + count < all.length ? String(start + count) : "0", keys: page } };
  });
  mock.on("GET", `${K}/{ns}/keys/{key}`, ({ params }) => {
    const it = space(params.ns!)[params.key!];
    if (!it) return nokey();
    const size = typeof it.value === "string" ? it.value.length : Array.isArray(it.value) ? it.value.length : Object.keys(it.value).length;
    return { status: 200, body: { key: params.key, type: it.type, ttl_ms: it.ttl_ms, size, truncated: false, value: it.value } };
  });
  mock.on("DELETE", `${K}/{ns}/keys/{key}`, ({ params }) => { const had = params.key! in space(params.ns!); delete space(params.ns!)[params.key!]; return { status: 200, body: { deleted: had } }; });
  mock.on("PUT", `${K}/{ns}/keys/{key}/ttl`, ({ params, body }) => { const it = space(params.ns!)[params.key!]; if (!it) return nokey(); it.ttl_ms = (body as { ttl_ms: number }).ttl_ms; return { status: 200, body: { applied: true } }; });
  mock.on("DELETE", `${K}/{ns}/keys/{key}/ttl`, ({ params }) => { const it = space(params.ns!)[params.key!]; if (!it) return nokey(); const had = it.ttl_ms !== null; it.ttl_ms = null; return { status: 200, body: { persisted: had } }; });
  mock.on("PUT", `${K}/{ns}/strings/{key}`, ({ params, body }) => {
    const b = body as { value: string; keepttl?: boolean };
    const prev = space(params.ns!)[params.key!];
    space(params.ns!)[params.key!] = { type: "string", value: b.value, ttl_ms: b.keepttl ? prev?.ttl_ms ?? null : null };
    return { status: 200, body: { stored: true } };
  });
  mock.on("POST", `${K}/{ns}/hashes/{key}/set`, ({ params, body }) => {
    const it = get(params.ns!, params.key!, "hash");
    if (it === "wrong") return wrong();
    const f = (body as { fields: Record<string, string> }).fields;
    if (!it) space(params.ns!)[params.key!] = { type: "hash", value: { ...f }, ttl_ms: null }; else Object.assign(it.value as Record<string, string>, f);
    return { status: 200, body: { added: Object.keys(f).length } };
  });
  mock.on("POST", `${K}/{ns}/hashes/{key}/delete`, ({ params, body }) => {
    const it = get(params.ns!, params.key!, "hash");
    if (!it || it === "wrong") return { status: 200, body: { removed: 0 } };
    for (const f of (body as { fields: string[] }).fields) delete (it.value as Record<string, string>)[f];
    if (Object.keys(it.value as object).length === 0) delete space(params.ns!)[params.key!];
    return { status: 200, body: { removed: 1 } };
  });
  mock.on("POST", `${K}/{ns}/lists/{key}/push`, ({ params, body }) => {
    const b = body as { side: "left" | "right"; values: string[] };
    let it = get(params.ns!, params.key!, "list");
    if (it === "wrong") return wrong();
    it ??= (space(params.ns!)[params.key!] = { type: "list", value: [], ttl_ms: null });
    const l = it.value as string[];
    if (b.side === "left") l.unshift(...b.values.reverse()); else l.push(...b.values);
    return { status: 200, body: { length: l.length } };
  });
  mock.on("POST", `${K}/{ns}/lists/{key}/pop`, ({ params, body }) => {
    const b = body as { side: "left" | "right"; count?: number };
    const it = get(params.ns!, params.key!, "list");
    if (!it || it === "wrong") return { status: 200, body: { values: [] } };
    const l = it.value as string[];
    const out = b.side === "left" ? l.splice(0, b.count ?? 1) : l.splice(-(b.count ?? 1)).reverse();
    if (l.length === 0) delete space(params.ns!)[params.key!];
    return { status: 200, body: { values: out } };
  });
  mock.on("POST", `${K}/{ns}/sets/{key}/add`, ({ params, body }) => {
    let it = get(params.ns!, params.key!, "set");
    if (it === "wrong") return wrong();
    it ??= (space(params.ns!)[params.key!] = { type: "set", value: [], ttl_ms: null });
    const l = it.value as string[];
    let added = 0;
    for (const m of (body as { members: string[] }).members) if (!l.includes(m)) { l.push(m); added++; }
    return { status: 200, body: { added } };
  });
  mock.on("POST", `${K}/{ns}/sets/{key}/remove`, ({ params, body }) => {
    const it = get(params.ns!, params.key!, "set");
    if (!it || it === "wrong") return { status: 200, body: { removed: 0 } };
    const rm = (body as { members: string[] }).members;
    it.value = (it.value as string[]).filter((m) => !rm.includes(m));
    if ((it.value as string[]).length === 0) delete space(params.ns!)[params.key!];
    return { status: 200, body: { removed: rm.length } };
  });
  mock.on("POST", `${K}/{ns}/zsets/{key}/add`, ({ params, body }) => {
    let it = get(params.ns!, params.key!, "zset");
    if (it === "wrong") return wrong();
    it ??= (space(params.ns!)[params.key!] = { type: "zset", value: [], ttl_ms: null });
    const l = it.value as { member: string; score: number }[];
    for (const m of (body as { members: { member: string; score: number }[] }).members) { const e = l.find((x) => x.member === m.member); if (e) e.score = m.score; else l.push({ ...m }); }
    l.sort((a, b) => a.score - b.score);
    return { status: 200, body: { added: 1 } };
  });
  mock.on("POST", `${K}/{ns}/zsets/{key}/remove`, ({ params, body }) => {
    const it = get(params.ns!, params.key!, "zset");
    if (!it || it === "wrong") return { status: 200, body: { removed: 0 } };
    const rm = (body as { members: string[] }).members;
    it.value = (it.value as { member: string }[]).filter((m) => !rm.includes(m.member)) as V;
    return { status: 200, body: { removed: rm.length } };
  });
  mock.on("POST", `${K}/{ns}/streams/{key}/add`, ({ params, body }) => {
    let it = get(params.ns!, params.key!, "stream");
    if (it === "wrong") return wrong();
    it ??= (space(params.ns!)[params.key!] = { type: "stream", value: [], ttl_ms: null });
    const l = it.value as { id: string; fields: Record<string, string> }[];
    const id = `${l.length + 2}-0`;
    l.push({ id, fields: (body as { fields: Record<string, string> }).fields });
    return { status: 201, body: { id } };
  });
  mock.on("POST", `${K}/{ns}/exec`, ({ params, body }) => {
    const cmd = (body as { command: string[] }).command;
    st.execs.push(cmd);
    const [name = "", a = "", b = "", c = ""] = cmd;
    const n = space(params.ns!);
    switch (name.toUpperCase()) {
      case "PING": return { status: 200, body: { result: "PONG" } };
      case "GET": { const it = n[a]; return { status: 200, body: { result: it?.type === "string" ? it.value : null } }; }
      case "SET": n[a] = { type: "string", value: b, ttl_ms: null }; return { status: 200, body: { result: "OK" } };
      case "KEYS": return { status: 200, body: { result: Object.keys(n).sort() } };
      case "LREM": { const it = n[a]; if (it?.type === "list") { const l = it.value as string[]; const i = l.indexOf(c ? c : b); if (i >= 0) l.splice(i, 1); } return { status: 200, body: { result: 1 } }; }
      case "XDEL": { const it = n[a]; if (it?.type === "stream") it.value = (it.value as { id: string }[]).filter((e) => e.id !== b) as V; return { status: 200, body: { result: 1 } }; }
      case "BOOM": return envelope(400, "kv_unknown_command", "unknown command 'BOOM'");
      default: return envelope(400, "kv_unknown_command", `unknown command '${name}'`);
    }
  });
  mock.on("POST", `${K}/{ns}/publish/{channel}`, ({ params, body }) => { st.published.push({ channel: params.channel!, message: (body as { message: unknown }).message }); return { status: 200, body: { receivers: 1 } }; });
  return st;
}
