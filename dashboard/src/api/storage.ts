/** Typed client for `/api/v1/storage` (buckets, ACLs, objects, uploads, versions, signed URLs, reconcile). Hand-written: sdk/js is owned elsewhere. */
import { ApiError, request, send } from "./client";
import { authHeaders } from "./token";

const S = "/api/v1/storage";
const enc = encodeURIComponent;
/** Object keys keep their `/` (the server route is a wildcard); every segment is escaped. */
export const encKey = (key: string) => key.split("/").map(enc).join("/");

export type Visibility = "private" | "authenticated" | "public";
export const VISIBILITIES: Visibility[] = ["private", "authenticated", "public"];
export type Permission = "read" | "write" | "admin";
export const PERMISSIONS: Permission[] = ["read", "write", "admin"];

export interface Bucket {
  name: string; visibility: Visibility; versioning: boolean; max_object_bytes?: number | null;
  allowed_content_types?: string[] | null; owner: string; created_at: string; updated_at: string;
}
export interface BucketInput { name: string; visibility?: Visibility; versioning?: boolean; max_object_bytes?: number | null; allowed_content_types?: string[] | null }
export type BucketPatch = Partial<Omit<BucketInput, "name">>;
export interface BucketAclEntry { prefix: string; grantee: string; permission: Permission }
export interface BucketAcl { entries: BucketAclEntry[] }
export interface ObjectInfo {
  bucket: string; key: string; version_id: string; size: number; content_type: string; checksum_sha256?: string | null;
  metadata: Record<string, string>; is_latest: boolean; delete_marker: boolean; created_by: string; created_at: string;
}
export interface ObjectPage { objects: ObjectInfo[]; prefixes: string[]; next_cursor?: string | null }
export interface DeleteResult { version_id: string; delete_marker: boolean }
export interface UploadSession {
  id: string; bucket: string; key: string; kind: string; content_type: string; declared_size?: number | null;
  declared_sha256?: string | null; received_bytes: number; parts: { part_number: number; size: number; checksum_sha256: string }[]; expires_at: string;
}
export interface SignedUrl { url: string; method: "GET" | "PUT"; expires_at: string }
export interface SignRequest { key: string; method?: "GET" | "PUT"; expires_in?: number; version_id?: string; download_as?: string; content_type?: string }
export interface ReconcileReport { uploads_aborted: number; staging_deleted: number; orphan_blobs_deleted: number; unreferenced_blobs_deleted: number; missing_blobs: string[] }
export interface StorageStats { buckets: number; objects: number; versions: number; logical_bytes: number; stored_bytes: number; open_uploads: number }
/** Space used by one bucket (`GET .../buckets/{bucket}/usage`): `objects`/`logical_bytes` count current objects, `versions`/`version_bytes` every version. */
export interface BucketUsage { bucket: string; objects: number; versions: number; logical_bytes: number; version_bytes: number }
export interface ObjectListOptions { prefix?: string; delimiter?: string; limit?: number; cursor?: string | null }

export const STORAGE_OPERATIONS = [
  "get /api/v1/storage/stats", "post /api/v1/storage/maintenance/reconcile", "get /api/v1/storage/buckets", "post /api/v1/storage/buckets",
  "patch /api/v1/storage/buckets/{bucket}", "delete /api/v1/storage/buckets/{bucket}", "get /api/v1/storage/buckets/{bucket}/acl",
  "put /api/v1/storage/buckets/{bucket}/acl", "get /api/v1/storage/buckets/{bucket}/objects", "put /api/v1/storage/buckets/{bucket}/objects/{key}",
  "delete /api/v1/storage/buckets/{bucket}/objects/{key}", "post /api/v1/storage/buckets/{bucket}/copy", "post /api/v1/storage/buckets/{bucket}/move",
  "get /api/v1/storage/buckets/{bucket}/versions/{key}", "post /api/v1/storage/buckets/{bucket}/restore", "post /api/v1/storage/buckets/{bucket}/sign",
  "post /api/v1/storage/buckets/{bucket}/uploads", "get /api/v1/storage/buckets/{bucket}/usage",
  "get /api/v1/storage/buckets/{bucket}/meta/{key}", "head /api/v1/storage/buckets/{bucket}/meta/{key}", "patch /api/v1/storage/uploads/{id}", "delete /api/v1/storage/uploads/{id}",
] as const;

export const storageStats = (signal?: AbortSignal) => request<StorageStats>(`${S}/stats`, { signal });
export const reconcileStorage = () => send<ReconcileReport>("POST", `${S}/maintenance/reconcile`);
export const listBuckets = async (signal?: AbortSignal) => (await request<{ items: Bucket[] }>(`${S}/buckets`, { signal })).items;
export const createBucket = (b: BucketInput) => send<Bucket>("POST", `${S}/buckets`, b);
export const getBucket = (name: string, signal?: AbortSignal) => request<Bucket>(`${S}/buckets/${enc(name)}`, { signal });
export const updateBucket = (name: string, p: BucketPatch) => send<Bucket>("PATCH", `${S}/buckets/${enc(name)}`, p);
export const deleteBucket = (name: string, force = false) => send<void>("DELETE", `${S}/buckets/${enc(name)}${force ? "?force=true" : ""}`);
export const getBucketAcl = (name: string, signal?: AbortSignal) => request<BucketAcl>(`${S}/buckets/${enc(name)}/acl`, { signal });
export const putBucketAcl = (name: string, acl: BucketAcl) => send<BucketAcl>("PUT", `${S}/buckets/${enc(name)}/acl`, acl);

export function listObjects(bucket: string, o: ObjectListOptions = {}, signal?: AbortSignal) {
  const q = new URLSearchParams();
  if (o.prefix) q.set("prefix", o.prefix);
  if (o.delimiter) q.set("delimiter", o.delimiter);
  if (o.limit) q.set("limit", String(o.limit));
  if (o.cursor) q.set("cursor", o.cursor);
  const s = q.toString();
  return request<ObjectPage>(`${S}/buckets/${enc(bucket)}/objects${s ? `?${s}` : ""}`, { signal });
}
/** Per-bucket usage (bucket admin). */
export const bucketUsage = (bucket: string, signal?: AbortSignal) => request<BucketUsage>(`${S}/buckets/${enc(bucket)}/usage`, { signal });
/** Metadata of one object (or one version) without its bytes. */
export const objectMeta = (bucket: string, key: string, version?: string, signal?: AbortSignal) =>
  request<ObjectInfo>(`${S}/buckets/${enc(bucket)}/meta/${encKey(key)}${version ? `?version=${enc(version)}` : ""}`, { signal });
export const deleteObject = (bucket: string, key: string, version?: string) =>
  send<DeleteResult>("DELETE", `${S}/buckets/${enc(bucket)}/objects/${encKey(key)}${version ? `?version=${enc(version)}` : ""}`);
export const copyObject = (bucket: string, key: string, destKey: string, destBucket?: string, versionId?: string) =>
  send<ObjectInfo>("POST", `${S}/buckets/${enc(bucket)}/copy`, { key, dest_key: destKey, ...(destBucket ? { dest_bucket: destBucket } : {}), ...(versionId ? { version_id: versionId } : {}) });
export const moveObject = (bucket: string, key: string, destKey: string, destBucket?: string) =>
  send<ObjectInfo>("POST", `${S}/buckets/${enc(bucket)}/move`, { key, dest_key: destKey, ...(destBucket ? { dest_bucket: destBucket } : {}) });
export const listVersions = async (bucket: string, key: string, signal?: AbortSignal) =>
  (await request<{ items: ObjectInfo[] }>(`${S}/buckets/${enc(bucket)}/versions/${encKey(key)}`, { signal })).items;
export const restoreVersion = (bucket: string, key: string, versionId: string) => send<ObjectInfo>("POST", `${S}/buckets/${enc(bucket)}/restore`, { key, version_id: versionId });
export const signUrl = (bucket: string, r: SignRequest) => send<SignedUrl>("POST", `${S}/buckets/${enc(bucket)}/sign`, r);

/** Bytes of an object (or of one version) through the authenticated API. */
export async function downloadObject(bucket: string, key: string, version?: string, signal?: AbortSignal): Promise<Blob> {
  const url = `${S}/buckets/${enc(bucket)}/objects/${encKey(key)}${version ? `?version=${enc(version)}` : ""}`;
  let res: Response;
  try {
    res = await fetch(url, { headers: await authHeaders(), signal, credentials: "same-origin" });
  } catch (err) {
    if (err instanceof DOMException && err.name === "AbortError") throw err;
    throw new ApiError({ status: 0, code: "network_error", message: err instanceof Error ? err.message : "Network request failed" });
  }
  if (!res.ok) {
    let code = "http_error", message = res.statusText || `HTTP ${res.status}`;
    try { const b = (await res.json()) as { error?: { code?: string; message?: string } }; if (b.error?.code) { code = b.error.code; message = b.error.message ?? message; } } catch { /* not JSON */ }
    throw new ApiError({ status: res.status, code, message });
  }
  return res.blob();
}

/** Files above this size go through a resumable session in chunks (with progress); smaller ones are one PUT. */
export const RESUMABLE_THRESHOLD = 8 * 1024 * 1024;
export const UPLOAD_CHUNK = 4 * 1024 * 1024;

export interface UploadOptions { onProgress?: (sent: number, total: number) => void; signal?: AbortSignal; chunkSize?: number; threshold?: number; contentType?: string }

/** Upload a file; resumable with progress when it is large. A chunk refused with `offset_mismatch` continues from the server's offset. */
export async function uploadObject(bucket: string, key: string, file: Blob, o: UploadOptions = {}): Promise<ObjectInfo> {
  const type = o.contentType || file.type || "application/octet-stream";
  const total = file.size;
  if (total <= (o.threshold ?? RESUMABLE_THRESHOLD)) {
    o.onProgress?.(0, total);
    const info = await request<ObjectInfo>(`${S}/buckets/${enc(bucket)}/objects/${encKey(key)}`, { method: "PUT", rawBody: file, headers: { "Content-Type": type }, signal: o.signal });
    o.onProgress?.(total, total);
    return info;
  }
  const chunk = o.chunkSize ?? UPLOAD_CHUNK;
  const session = await send<UploadSession>("POST", `${S}/buckets/${enc(bucket)}/uploads`, { key, kind: "resumable", content_type: type, size: total });
  let offset = session.received_bytes;
  o.onProgress?.(offset, total);
  try {
    for (;;) {
      o.signal?.throwIfAborted();
      const end = Math.min(offset + chunk, total);
      let out: ObjectInfo | UploadSession;
      try {
        out = await request<ObjectInfo | UploadSession>(`${S}/uploads/${enc(session.id)}`, {
          method: "PATCH", rawBody: file.slice(offset, end), headers: { "Content-Type": "application/offset+octet-stream", "Upload-Offset": String(offset) }, signal: o.signal,
        });
      } catch (e) {
        const server = e instanceof ApiError && e.code === "offset_mismatch" ? (e.body as { error?: { details?: { upload_offset?: number } } } | undefined)?.error?.details?.upload_offset : undefined;
        if (typeof server === "number" && server !== offset) { offset = server; o.onProgress?.(offset, total); continue; }
        throw e;
      }
      if ("version_id" in out) { o.onProgress?.(total, total); return out; }
      offset = out.received_bytes;
      o.onProgress?.(offset, total);
      if (offset >= total) throw new ApiError({ status: 0, code: "upload_incomplete", message: "The server did not finish the upload" });
    }
  } catch (e) {
    await send<void>("DELETE", `${S}/uploads/${enc(session.id)}`).catch(() => undefined);
    throw e;
  }
}

/** Byte limit of the `storage.bytes` entitlement for the caller's subject (`null` = unlimited or not reported). */
export async function storageQuotaBytes(signal?: AbortSignal): Promise<number | null> {
  const e = await request<{ features?: Record<string, { enabled?: boolean; limit?: number | null }> }>("/api/v1/entitlements", { signal });
  const g = e.features?.["storage.bytes"];
  return g && typeof g.limit === "number" ? g.limit : null;
}
