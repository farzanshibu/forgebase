/** In-memory fake of `/api/v1/storage` (and the entitlements read the usage panel makes) for component tests. Routes the OpenAPI fixture describes are contract-checked: an undocumented route/status or a body that does not fit its schema lands in `mock.problems`. */
import type { BucketAcl, BucketUsage, Bucket, ObjectInfo, ReconcileReport, StorageStats, UploadSession } from "../api";
import { envelope, type MockApi, type MockReply, type MockRequest } from "./mockApi";
import { hasResponse, matchRoute, responseSchemaName, validateBody } from "./contract";

/** Registers a route and, unless `check` is false, validates every answer against the OpenAPI fixture (status documented, JSON body fits the schema). */
function checkedOn(mock: MockApi) {
  return (method: string, template: string, h: (r: MockRequest) => MockReply, check = true) =>
    mock.on(method, template, (req) => {
      const out = h(req);
      if (!check) return out;
      const path = template.replace(/\{\*?(\w+)\}/g, (_, n: string) => encodeURIComponent(req.params[n] ?? ""));
      const spec = matchRoute(method, path);
      if (!spec) mock.problems.push(`not in openapi.json: ${method} ${template}`);
      else if (out.status < 400) {
        if (!hasResponse(spec.op, out.status)) mock.problems.push(`${method} ${template}: status ${out.status} not documented`);
        const name = responseSchemaName(spec.op, out.status);
        if (name && out.body !== undefined) for (const p of validateBody(name, out.body)) mock.problems.push(`${method} ${template} -> ${name}: ${p}`);
      }
      return out;
    });
}


const NOW = "2026-11-09T10:00:00Z";
export const bucket = (name: string, extra: Partial<Bucket> = {}): Bucket => ({ name, visibility: "private", versioning: false, max_object_bytes: null, allowed_content_types: null, owner: "admin", created_at: NOW, updated_at: NOW, ...extra });
export const obj = (bucketName: string, key: string, size: number, extra: Partial<ObjectInfo> = {}): ObjectInfo => ({
  bucket: bucketName, key, version_id: `v_${key}_1`, size, content_type: "text/plain", checksum_sha256: "ab".repeat(32), metadata: {}, is_latest: true, delete_marker: false, created_by: "admin", created_at: NOW, ...extra,
});

export interface StorageState {
  buckets: Bucket[];
  /** Every version of every object, newest first per key. */
  versions: Record<string, ObjectInfo[]>;
  acls: Record<string, BucketAcl>;
  uploads: Record<string, UploadSession & { buffer: number }>;
  quota: number | null;
  report: ReconcileReport;
  /** Offsets the server pretends to hold when a chunk arrives (to exercise offset_mismatch). */
  forceOffset: number | null;
  signed: unknown[];
  puts: { key: string; size: number; type: string }[];
  chunks: { offset: number; size: number }[];
}

export function installStorage(mockApi: MockApi): StorageState {
  const mock = { on: checkedOn(mockApi) };
  const st: StorageState = {
    buckets: [bucket("media", { versioning: true }), bucket("docs")],
    versions: {
      "media/photos/a.png": [obj("media", "photos/a.png", 2048, { content_type: "image/png", version_id: "v2", created_at: "2026-11-09T11:00:00Z" }), obj("media", "photos/a.png", 1024, { content_type: "image/png", version_id: "v1", is_latest: false })],
      "media/photos/b.png": [obj("media", "photos/b.png", 10, { content_type: "image/png" })],
      "media/readme.txt": [obj("media", "readme.txt", 5, { metadata: { author: "kim" } })],
      "docs/guide.md": [obj("docs", "guide.md", 300, { content_type: "text/markdown" })],
    },
    acls: { media: { entries: [{ prefix: "photos/", grantee: "role:editor", permission: "write" }] } },
    uploads: {},
    quota: 10 * 1024 * 1024 * 1024,
    report: { uploads_aborted: 1, staging_deleted: 2, orphan_blobs_deleted: 3, unreferenced_blobs_deleted: 4, missing_blobs: [] },
    forceOffset: null,
    signed: [],
    puts: [],
    chunks: [],
  };
  const S = "/api/v1/storage";
  const nf = (what = "bucket") => envelope(404, "not_found", `no such ${what}`);
  const find = (n: string) => st.buckets.find((b) => b.name === n);
  const live = (b: string) => Object.entries(st.versions).filter(([k]) => k.startsWith(`${b}/`)).map(([, vs]) => vs[0]!).filter((o) => o.is_latest && !o.delete_marker);
  const stats = (): StorageStats => {
    const all = Object.values(st.versions).flat();
    const l = all.filter((o) => o.is_latest && !o.delete_marker);
    return { buckets: st.buckets.length, objects: l.length, versions: all.length, logical_bytes: l.reduce((n, o) => n + o.size, 0), stored_bytes: all.reduce((n, o) => n + o.size, 0), open_uploads: Object.keys(st.uploads).length };
  };
  const vkey = (b: string, k: string) => `${b}/${k}`;
  const put = (b: string, key: string, size: number, type: string): ObjectInfo => {
    const bk = find(b)!;
    const prev = st.versions[vkey(b, key)] ?? [];
    const o = obj(b, key, size, { content_type: type, version_id: `v${prev.length + 1}_${key}` });
    prev.forEach((p) => (p.is_latest = false));
    st.versions[vkey(b, key)] = bk.versioning ? [o, ...prev] : [o];
    return o;
  };

  mock.on("GET", `${S}/stats`, () => ({ status: 200, body: stats() }));
  mock.on("POST", `${S}/maintenance/reconcile`, () => ({ status: 200, body: st.report }));
  mock.on("GET", "/api/v1/entitlements", () => ({ status: 200, body: { subject: "user:admin", plan: "pro", entitled: true, features: st.quota === null ? {} : { "storage.bytes": { enabled: true, kind: "quota", limit: st.quota, unit: "bytes", period: "none", source: "plan" } } } }), false);
  mock.on("GET", `${S}/buckets`, () => ({ status: 200, body: { items: st.buckets } }));
  mock.on("POST", `${S}/buckets`, ({ body }) => {
    const b = body as { name: string; visibility?: Bucket["visibility"]; versioning?: boolean; max_object_bytes?: number | null };
    if (!/^[a-z0-9][a-z0-9-]{2,}$/.test(b.name)) return envelope(400, "invalid_bucket_name", "bucket names are 3+ lowercase letters, digits or dashes");
    if (find(b.name)) return envelope(409, "bucket_exists", "bucket already exists");
    const nb = bucket(b.name, { visibility: b.visibility ?? "private", versioning: !!b.versioning, max_object_bytes: b.max_object_bytes ?? null });
    st.buckets.push(nb);
    return { status: 201, body: nb };
  });
  mock.on("GET", `${S}/buckets/{bucket}`, ({ params }) => { const b = find(params.bucket!); return b ? { status: 200, body: b } : nf(); });
  mock.on("PATCH", `${S}/buckets/{bucket}`, ({ params, body }) => {
    const b = find(params.bucket!);
    if (!b) return nf();
    Object.assign(b, Object.fromEntries(Object.entries(body as object).filter(([, v]) => v !== undefined)));
    return { status: 200, body: b };
  });
  mock.on("DELETE", `${S}/buckets/{bucket}`, ({ params, query }) => {
    const b = find(params.bucket!);
    if (!b) return nf();
    if (live(b.name).length > 0 && query.get("force") !== "true") return envelope(409, "bucket_not_empty", "the bucket still holds objects");
    st.buckets = st.buckets.filter((x) => x !== b);
    for (const k of Object.keys(st.versions)) if (k.startsWith(`${b.name}/`)) delete st.versions[k];
    return { status: 204 };
  });
  mock.on("GET", `${S}/buckets/{bucket}/acl`, ({ params }) => (find(params.bucket!) ? { status: 200, body: st.acls[params.bucket!] ?? { entries: [] } } : nf()));
  mock.on("PUT", `${S}/buckets/{bucket}/acl`, ({ params, body }) => { st.acls[params.bucket!] = body as BucketAcl; return { status: 200, body }; });
  mock.on("GET", `${S}/buckets/{bucket}/objects`, ({ params, query }) => {
    if (!find(params.bucket!)) return nf();
    const prefix = query.get("prefix") ?? "", delim = query.get("delimiter");
    const limit = Number(query.get("limit") ?? 100), start = Number(query.get("cursor") ?? 0);
    const keys = live(params.bucket!).filter((o) => o.key.startsWith(prefix)).sort((a, b) => a.key.localeCompare(b.key));
    const prefixes = new Set<string>(), objects: ObjectInfo[] = [];
    for (const o of keys) {
      const rest = o.key.slice(prefix.length), i = delim ? rest.indexOf(delim) : -1;
      if (i >= 0) prefixes.add(prefix + rest.slice(0, i + 1)); else objects.push(o);
    }
    const page = objects.slice(start, start + limit);
    return { status: 200, body: { objects: page, prefixes: [...prefixes], next_cursor: start + limit < objects.length ? String(start + limit) : null } };
  });
  mock.on("PUT", `${S}/buckets/{bucket}/objects/{*key}`, ({ params, body, headers }) => {
    const b = find(params.bucket!);
    if (!b) return nf();
    const size = body instanceof Blob ? body.size : 0;
    if (b.max_object_bytes && size > b.max_object_bytes) return envelope(413, "object_too_large", "object exceeds the bucket limit");
    const type = headers["Content-Type"] ?? "";
    st.puts.push({ key: params.key!, size, type });
    return { status: 201, body: put(b.name, params.key!, size, type) };
  });
  mock.on("GET", `${S}/buckets/{bucket}/objects/{*key}`, ({ params, query }) => {
    const vs = st.versions[vkey(params.bucket!, params.key!)];
    const v = query.get("version") ? vs?.find((x) => x.version_id === query.get("version")) : vs?.[0];
    return v && !v.delete_marker ? { status: 200, body: `bytes of ${v.version_id}` } : envelope(404, "object_not_found", "no such object");
  });
  mock.on("DELETE", `${S}/buckets/{bucket}/objects/{*key}`, ({ params, query }) => {
    const k = vkey(params.bucket!, params.key!), vs = st.versions[k];
    if (!vs) return envelope(404, "object_not_found", "no such object");
    if (query.get("version")) {
      st.versions[k] = vs.filter((v) => v.version_id !== query.get("version"));
      if (st.versions[k]!.length === 0) delete st.versions[k]; else st.versions[k]![0]!.is_latest = true;
      return { status: 200, body: { version_id: query.get("version"), delete_marker: false } };
    }
    if (find(params.bucket!)!.versioning) {
      vs.forEach((v) => (v.is_latest = false));
      const m = obj(params.bucket!, params.key!, 0, { version_id: `dm${vs.length}`, delete_marker: true });
      st.versions[k] = [m, ...vs];
      return { status: 200, body: { version_id: m.version_id, delete_marker: true } };
    }
    delete st.versions[k];
    return { status: 200, body: { version_id: vs[0]!.version_id, delete_marker: false } };
  });
  const xfer = (move: boolean) => ({ params, body }: MockRequest) => {
    const b = body as { key: string; dest_key: string; dest_bucket?: string };
    const src = st.versions[vkey(params.bucket!, b.key)]?.[0];
    if (!src) return envelope(404, "object_not_found", "no such object");
    const dest = put(b.dest_bucket ?? params.bucket!, b.dest_key, src.size, src.content_type);
    if (move) delete st.versions[vkey(params.bucket!, b.key)];
    return { status: move ? 200 : 201, body: dest };
  };
  mock.on("GET", `${S}/buckets/{bucket}/usage`, ({ params }) => {
    if (!find(params.bucket!)) return nf();
    const all = Object.entries(st.versions).filter(([k]) => k.startsWith(`${params.bucket}/`)).flatMap(([, vs]) => vs);
    const l = live(params.bucket!);
    const body: BucketUsage = { bucket: params.bucket!, objects: l.length, versions: all.length, logical_bytes: l.reduce((n, o) => n + o.size, 0), version_bytes: all.reduce((n, o) => n + o.size, 0) };
    return { status: 200, body };
  });
  mock.on("GET", `${S}/buckets/{bucket}/meta/{*key}`, ({ params, query }) => {
    const vs = st.versions[vkey(params.bucket!, params.key!)];
    const v = query.get("version") ? vs?.find((x) => x.version_id === query.get("version")) : vs?.[0];
    return v && !v.delete_marker ? { status: 200, body: v } : envelope(404, "object_not_found", "no such object");
  });
  mock.on("POST", `${S}/buckets/{bucket}/copy`, xfer(false));
  mock.on("POST", `${S}/buckets/{bucket}/move`, xfer(true));
  // Unchecked: the fixture documents this answer as a bare ObjectInfo, but the server returns `{items}` (documentation bug, see report).
  mock.on("GET", `${S}/buckets/{bucket}/versions/{*key}`, ({ params }) => ({ status: 200, body: { items: st.versions[vkey(params.bucket!, params.key!)] ?? [] } }), false);
  mock.on("POST", `${S}/buckets/{bucket}/restore`, ({ params, body }) => {
    const b = body as { key: string; version_id: string };
    const src = st.versions[vkey(params.bucket!, b.key)]?.find((v) => v.version_id === b.version_id);
    return src ? { status: 201, body: put(params.bucket!, b.key, src.size, src.content_type) } : envelope(404, "version_not_found", "no such version");
  });
  mock.on("POST", `${S}/buckets/{bucket}/sign`, ({ params, body }) => {
    st.signed.push(body);
    const b = body as { key: string; method?: string; expires_in?: number };
    return { status: 200, body: { url: `https://fb.test/api/v1/storage/signed/${params.bucket}/${b.key}?expires=${b.expires_in}&sig=abc`, method: b.method ?? "GET", expires_at: "2026-11-09T11:00:00Z" } };
  });
  mock.on("POST", `${S}/buckets/{bucket}/uploads`, ({ params, body }) => {
    const b = body as { key: string; kind?: string; size?: number; content_type?: string };
    const up = { id: `up${Object.keys(st.uploads).length + 1}`, bucket: params.bucket!, key: b.key, kind: b.kind ?? "multipart", content_type: b.content_type ?? "", declared_size: b.size ?? null, declared_sha256: null, received_bytes: 0, parts: [], expires_at: NOW, buffer: 0 };
    st.uploads[up.id] = up;
    return { status: 201, body: up };
  });
  mock.on("PATCH", `${S}/uploads/{id}`, ({ params, body, headers }) => {
    const up = st.uploads[params.id!];
    if (!up) return envelope(404, "upload_not_found", "no such upload");
    const offset = Number(headers["Upload-Offset"]);
    if (st.forceOffset !== null) { const f = st.forceOffset; st.forceOffset = null; up.received_bytes = f; return envelope(409, "offset_mismatch", "wrong offset", { upload_offset: f }); }
    if (offset !== up.received_bytes) return envelope(409, "offset_mismatch", "wrong offset", { upload_offset: up.received_bytes });
    const size = body instanceof Blob ? body.size : 0;
    st.chunks.push({ offset, size });
    up.received_bytes += size;
    if (up.received_bytes >= (up.declared_size ?? Infinity)) {
      delete st.uploads[up.id];
      return { status: 201, body: put(up.bucket, up.key, up.received_bytes, up.content_type) };
    }
    return { status: 200, body: up };
  });
  mock.on("DELETE", `${S}/uploads/{id}`, ({ params }) => { delete st.uploads[params.id!]; return { status: 204 }; });
  return st;
}
