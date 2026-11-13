import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Object storage (`/api/v1/storage`): `client.storage`.
 *
 *   const avatars = fb.storage.from("avatars");
 *   await avatars.put("u1/me.png", file, { contentType: "image/png" });
 *   const { url } = await avatars.createSignedUrl("u1/me.png", { expiresIn: 600 });
 */

export type Bucket = Schemas["Bucket"];
export type BucketUsage = Schemas["BucketUsage"];
export type BucketVisibility = Schemas["Visibility"];
export type BucketPermission = Schemas["Permission"];
export type BucketAcl = Schemas["Acl"];
export type BucketAclEntry = Schemas["AclEntry"];
export type StorageObject = Schemas["ObjectInfo"];
export type ObjectPage = Schemas["ObjectPage"];
export type DeleteResult = Schemas["DeleteResult"];
export type Upload = Schemas["Upload"];
export type UploadPart = Schemas["PartInfo"];
export type SignedUrl = Schemas["SignedUrl"];
export type StorageStats = Schemas["StorageStats"];
export type ReconcileReport = Schemas["ReconcileReport"];

/** What a body can be: anything `fetch` sends as is (a `ReadableStream` is not supported, see the README). */
export type UploadBody = Blob | ArrayBuffer | ArrayBufferView | string;

export interface CreateBucketInput {
  name: string;
  /** Default read access (default `private`). */
  visibility?: BucketVisibility;
  /** Keep earlier versions on overwrite and delete (default off). */
  versioning?: boolean;
  /** Allowed content types; `image/*` patterns work. */
  allowedContentTypes?: string[];
  /** Largest object in bytes. */
  maxObjectBytes?: number;
}

export interface UpdateBucketInput {
  visibility?: BucketVisibility;
  versioning?: boolean;
  /** An empty list clears the allow list. */
  allowedContentTypes?: string[];
  /** `0` clears the limit. */
  maxObjectBytes?: number;
}

export interface PutOptions {
  contentType?: string;
  /** User metadata, stored with the object and returned on download as `X-ForgeBase-Meta-<name>`. */
  metadata?: Record<string, string>;
  /** Hex SHA-256 of the content (see `sha256Hex`); the server rejects the upload on a mismatch (`422 checksum_mismatch`). */
  checksumSha256?: string;
  signal?: AbortSignal;
}

export interface ListObjectsOptions {
  prefix?: string;
  /** Group keys on this character (`"/"`) into `prefixes`. */
  delimiter?: string;
  limit?: number;
  cursor?: string;
  signal?: AbortSignal;
}

/** A byte range of a download. */
export type ByteRange = string | { start: number; end?: number } | { suffix: number };

export interface DownloadOptions {
  /** A specific version. */
  version?: string;
  /** Offer this file name (`Content-Disposition: attachment`). */
  downloadAs?: string;
  /** `bytes=0-99`, `{ start: 0, end: 99 }` (inclusive) or `{ suffix: 100 }` (the last 100 bytes). */
  range?: ByteRange;
  signal?: AbortSignal;
}

export interface SignOptions {
  /** `GET` (default) or `PUT`. */
  method?: "GET" | "PUT";
  /** Lifetime in seconds (default 900). */
  expiresIn?: number;
  /** Uploads: the content type the upload must declare. */
  contentType?: string;
  /** Downloads: file name to offer. */
  downloadAs?: string;
  /** Downloads: a specific version. */
  versionId?: string;
}

export interface CopyOptions {
  /** Destination bucket (default: the same). */
  destBucket?: string;
  /** Source version (copy only). */
  versionId?: string;
}

export interface CreateUploadOptions {
  /** `multipart` (numbered parts, any order; default) or `resumable` (sequential offsets). */
  kind?: "multipart" | "resumable";
  contentType?: string;
  metadata?: Record<string, string>;
  /** Total size; resumable uploads complete themselves when it is reached. */
  size?: number;
  /** SHA-256 of the whole object, verified on completion. */
  checksumSha256?: string;
}

export interface CompleteUploadOptions {
  /** Parts to assemble, in order (default: every received part in part-number order). */
  parts?: { partNumber: number; checksumSha256?: string }[];
  checksumSha256?: string;
}

export interface UploadProgress {
  uploadedBytes: number;
  totalBytes: number;
}

export interface AutoUploadOptions extends Omit<CreateUploadOptions, "kind" | "size"> {
  /** Part size in bytes (default 8 MiB). */
  partSize?: number;
  onProgress?: (p: UploadProgress) => void;
  signal?: AbortSignal;
}

/** Result of appending a chunk to a resumable upload. */
export interface AppendResult {
  /** Where the next chunk starts. */
  offset: number;
  /** The session, while the upload is still open. */
  upload?: Upload;
  /** The finished object, once the declared size was reached. */
  object?: StorageObject;
}

const seg = encodeURIComponent;
/** A key in a URL path: slashes stay, every segment is encoded. */
const keyPath = (key: string) => key.split("/").map(seg).join("/");
const META = "X-Forgebase-Meta-";

const DEFAULT_PART = 8 * 1024 * 1024;

/** Hex SHA-256 of `data` (Web Crypto), for `checksumSha256`. */
export async function sha256Hex(data: string | ArrayBuffer | ArrayBufferView | Blob): Promise<string> {
  let bytes: ArrayBuffer | ArrayBufferView;
  if (typeof data === "string") bytes = new TextEncoder().encode(data);
  else if (typeof Blob !== "undefined" && data instanceof Blob) bytes = await data.arrayBuffer();
  else bytes = data as ArrayBuffer | ArrayBufferView;
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes as BufferSource);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

function rangeHeader(r: ByteRange): string {
  if (typeof r === "string") return r;
  if ("suffix" in r) return `bytes=-${r.suffix}`;
  return `bytes=${r.start}-${r.end ?? ""}`;
}

function uploadHeaders(o: { contentType?: string; metadata?: Record<string, string>; checksumSha256?: string }): Record<string, string> {
  const h: Record<string, string> = {};
  if (o.contentType) h["Content-Type"] = o.contentType;
  if (o.checksumSha256) h["X-Forgebase-Checksum-Sha256"] = o.checksumSha256;
  for (const [k, v] of Object.entries(o.metadata ?? {})) h[`${META}${k}`] = v;
  return h;
}

const sizeOf = (d: UploadBody): number => (typeof d === "string" ? new TextEncoder().encode(d).byteLength : d instanceof Blob ? d.size : d.byteLength);

/** A byte slice of `d` as something `fetch` sends as is. */
function slice(d: UploadBody, from: number, to: number): Blob | ArrayBuffer | ArrayBufferView | string {
  if (typeof d === "string") return new TextEncoder().encode(d).slice(from, to);
  if (d instanceof Blob) return d.slice(from, to);
  if (ArrayBuffer.isView(d)) return new Uint8Array(d.buffer, d.byteOffset + from, Math.min(to, d.byteLength) - from);
  return d.slice(from, to);
}

/** Buckets: `client.storage.buckets`. */
export class BucketsApi {
  constructor(private readonly t: Transport) {}

  /** Buckets the caller may read. */
  async list(): Promise<Bucket[]> {
    return (await this.t.request<{ items: Bucket[] }>("/api/v1/storage/buckets")).items;
  }

  get(name: string): Promise<Bucket> {
    return this.t.request(`/api/v1/storage/buckets/${seg(name)}`);
  }

  create(input: CreateBucketInput): Promise<Bucket> {
    return this.t.request("/api/v1/storage/buckets", {
      method: "POST",
      body: {
        name: input.name,
        visibility: input.visibility,
        versioning: input.versioning,
        allowed_content_types: input.allowedContentTypes,
        max_object_bytes: input.maxObjectBytes,
      },
    });
  }

  /** Changes a bucket's policy. */
  update(name: string, patch: UpdateBucketInput): Promise<Bucket> {
    return this.t.request(`/api/v1/storage/buckets/${seg(name)}`, {
      method: "PATCH",
      body: {
        visibility: patch.visibility,
        versioning: patch.versioning,
        allowed_content_types: patch.allowedContentTypes,
        max_object_bytes: patch.maxObjectBytes,
      },
    });
  }

  /** Deletes a bucket; `force: true` removes its objects too. */
  delete(name: string, o: { force?: boolean } = {}): Promise<void> {
    return this.t.request(`/api/v1/storage/buckets/${seg(name)}`, { method: "DELETE", query: { force: o.force } });
  }

  /** The ACL of a bucket (needs admin access on it). */
  acl(name: string): Promise<BucketAcl> {
    return this.t.request(`/api/v1/storage/buckets/${seg(name)}/acl`);
  }

  /** Replaces the whole ACL. */
  setAcl(name: string, entries: BucketAclEntry[]): Promise<BucketAcl> {
    return this.t.request(`/api/v1/storage/buckets/${seg(name)}/acl`, { method: "PUT", body: { entries } });
  }

  /** Space used by a bucket. */
  usage(name: string): Promise<BucketUsage> {
    return this.t.request(`/api/v1/storage/buckets/${seg(name)}/usage`);
  }
}

/** The objects of one bucket: `client.storage.from("avatars")`. */
export class BucketApi {
  constructor(
    private readonly t: Transport,
    readonly bucket: string,
  ) {}

  private path(suffix = ""): string {
    return `/api/v1/storage/buckets/${seg(this.bucket)}${suffix}`;
  }

  /** Current objects under `prefix`, one page. */
  list(o: ListObjectsOptions = {}): Promise<ObjectPage> {
    return this.t.request(this.path("/objects"), {
      signal: o.signal,
      query: { prefix: o.prefix, delimiter: o.delimiter, limit: o.limit, cursor: o.cursor },
    });
  }

  /** Every object under `prefix`, lazily, following the cursor. */
  async *iterate(o: Omit<ListObjectsOptions, "cursor"> = {}): AsyncGenerator<StorageObject, void, undefined> {
    let cursor: string | undefined;
    for (;;) {
      const page = await this.list({ ...o, cursor });
      yield* page.objects;
      if (!page.next_cursor) return;
      cursor = page.next_cursor;
    }
  }

  /** Uploads an object in one request (`PUT`). Overwrites the key (a new version when the bucket is versioned). */
  put(key: string, body: UploadBody, o: PutOptions = {}): Promise<StorageObject> {
    return this.t.request(this.path(`/objects/${keyPath(key)}`), {
      method: "PUT",
      rawBody: body as BodyInit,
      headers: uploadHeaders(o),
      signal: o.signal,
    });
  }

  /** Downloads an object: the raw `Response`, so the body can be streamed. A range answers `206`. */
  download(key: string, o: DownloadOptions = {}): Promise<Response> {
    return this.t.request<Response>(this.path(`/objects/${keyPath(key)}`), {
      raw: true,
      signal: o.signal,
      query: { version: o.version, download: o.downloadAs },
      headers: o.range === undefined ? undefined : { Range: rangeHeader(o.range) },
    });
  }

  /** The object as text. */
  async text(key: string, o: DownloadOptions = {}): Promise<string> {
    return (await this.download(key, o)).text();
  }

  /** The object parsed as JSON. */
  async json<T = unknown>(key: string, o: DownloadOptions = {}): Promise<T> {
    return (await this.download(key, o)).json() as Promise<T>;
  }

  /** The object as a `Blob`. */
  async blob(key: string, o: DownloadOptions = {}): Promise<Blob> {
    return (await this.download(key, o)).blob();
  }

  /** The object as bytes. */
  async bytes(key: string, o: DownloadOptions = {}): Promise<Uint8Array> {
    return new Uint8Array(await (await this.download(key, o)).arrayBuffer());
  }

  /** Metadata of an object (or of one `version`) without reading its bytes. */
  meta(key: string, o: { version?: string; signal?: AbortSignal } = {}): Promise<StorageObject> {
    return this.t.request(this.path(`/meta/${keyPath(key)}`), { query: { version: o.version }, signal: o.signal });
  }

  /** Deletes an object (a delete marker in a versioned bucket), or one version with `version`. */
  delete(key: string, o: { version?: string } = {}): Promise<DeleteResult> {
    return this.t.request(this.path(`/objects/${keyPath(key)}`), { method: "DELETE", query: { version: o.version } });
  }

  /** Copies an object (no bytes move). */
  copy(key: string, destKey: string, o: CopyOptions = {}): Promise<StorageObject> {
    return this.t.request(this.path("/copy"), {
      method: "POST",
      body: { key, dest_key: destKey, dest_bucket: o.destBucket, version_id: o.versionId },
    });
  }

  /** Moves (renames) an object. */
  move(key: string, destKey: string, o: { destBucket?: string } = {}): Promise<StorageObject> {
    return this.t.request(this.path("/move"), { method: "POST", body: { key, dest_key: destKey, dest_bucket: o.destBucket } });
  }

  /** Every version of a key, newest first. */
  async versions(key: string): Promise<StorageObject[]> {
    return (await this.t.request<{ items: StorageObject[] }>(this.path(`/versions/${keyPath(key)}`))).items;
  }

  /** Restores an earlier version as a new current version. */
  restore(key: string, versionId: string): Promise<StorageObject> {
    return this.t.request(this.path("/restore"), { method: "POST", body: { key, version_id: versionId } });
  }

  /** A signed URL (relative to the server root; see `storage.signed.resolve`). */
  sign(key: string, o: SignOptions = {}): Promise<SignedUrl> {
    return this.t.request(this.path("/sign"), {
      method: "POST",
      body: {
        key,
        method: o.method,
        expires_in: o.expiresIn,
        content_type: o.contentType,
        download_as: o.downloadAs,
        version_id: o.versionId,
      },
    });
  }

  /** A download link anyone holding it can use until it expires. */
  createSignedUrl(key: string, o: Omit<SignOptions, "method" | "contentType"> = {}): Promise<SignedUrl> {
    return this.sign(key, { ...o, method: "GET" });
  }

  /** An upload link: `PUT` the content to it (`storage.signed.upload`). */
  createSignedUploadUrl(key: string, o: Omit<SignOptions, "method" | "downloadAs" | "versionId"> = {}): Promise<SignedUrl> {
    return this.sign(key, { ...o, method: "PUT" });
  }
}

/** Multipart and resumable upload sessions: `client.storage.uploads`. */
export class UploadsApi {
  constructor(private readonly t: Transport) {}

  /** Opens a session for `key`. */
  create(bucket: string, key: string, o: CreateUploadOptions = {}): Promise<Upload> {
    return this.t.request(`/api/v1/storage/buckets/${seg(bucket)}/uploads`, {
      method: "POST",
      body: {
        key,
        kind: o.kind,
        content_type: o.contentType,
        metadata: o.metadata,
        size: o.size,
        checksum_sha256: o.checksumSha256,
      },
    });
  }

  /** State of a session: `received_bytes` is where a resumable client continues. */
  get(id: string): Promise<Upload> {
    return this.t.request(`/api/v1/storage/uploads/${seg(id)}`);
  }

  /** Cancels a session. */
  abort(id: string): Promise<void> {
    return this.t.request(`/api/v1/storage/uploads/${seg(id)}`, { method: "DELETE" });
  }

  /** Uploads one numbered part (1 to 10000) of a multipart session. A part can be sent again. */
  putPart(id: string, partNumber: number, body: UploadBody, o: { checksumSha256?: string; signal?: AbortSignal } = {}): Promise<UploadPart> {
    return this.t.request(`/api/v1/storage/uploads/${seg(id)}/parts/${partNumber}`, {
      method: "PUT",
      rawBody: body as BodyInit,
      headers: uploadHeaders({ checksumSha256: o.checksumSha256, contentType: "application/octet-stream" }),
      signal: o.signal,
    });
  }

  /** Assembles the parts into the object. */
  complete(id: string, o: CompleteUploadOptions = {}): Promise<StorageObject> {
    return this.t.request(`/api/v1/storage/uploads/${seg(id)}/complete`, {
      method: "POST",
      body: {
        parts: o.parts?.map((p) => ({ part_number: p.partNumber, checksum_sha256: p.checksumSha256 })),
        checksum_sha256: o.checksumSha256,
      },
    });
  }

  /**
   * Appends a chunk to a resumable session at `offset` (a wrong offset is a `ConflictError` with code
   * `offset_mismatch` and `details.upload_offset`). The upload finishes by itself at the declared size.
   */
  async append(id: string, offset: number, chunk: UploadBody, o: { checksumSha256?: string; signal?: AbortSignal } = {}): Promise<AppendResult> {
    const res = await this.t.request<Response>(`/api/v1/storage/uploads/${seg(id)}`, {
      method: "PATCH",
      raw: true,
      rawBody: chunk as BodyInit,
      headers: { ...uploadHeaders({ checksumSha256: o.checksumSha256, contentType: "application/octet-stream" }), "Upload-Offset": String(offset) },
      signal: o.signal,
    });
    const body = (await res.json()) as Upload | StorageObject;
    const next = Number(res.headers.get("upload-offset") ?? NaN);
    if (res.status === 201) return { offset: Number.isNaN(next) ? sizeOf(chunk) + offset : next, object: body as StorageObject };
    const upload = body as Upload;
    return { offset: Number.isNaN(next) ? upload.received_bytes : next, upload };
  }

  /**
   * Uploads `data` through a multipart session in parts of `partSize` bytes, then completes it. Aborts the session
   * when a part fails. For a file that fits in one request use `from(bucket).put`.
   */
  async upload(bucket: string, key: string, data: UploadBody, o: AutoUploadOptions = {}): Promise<StorageObject> {
    const partSize = Math.max(1, o.partSize ?? DEFAULT_PART);
    const total = sizeOf(data);
    const session = await this.create(bucket, key, { kind: "multipart", contentType: o.contentType, metadata: o.metadata, checksumSha256: o.checksumSha256 });
    try {
      let n = 0;
      let sent = 0;
      for (let from = 0; from < total || n === 0; from += partSize) {
        const to = Math.min(total, from + partSize);
        await this.putPart(session.id, ++n, slice(data, from, to), { signal: o.signal });
        sent = to;
        o.onProgress?.({ uploadedBytes: sent, totalBytes: total });
      }
      return await this.complete(session.id);
    } catch (err) {
      await this.abort(session.id).catch(() => undefined);
      throw err;
    }
  }

  /**
   * Sends what a resumable session is still missing: reads the server's offset, then uploads `data` from there in
   * chunks. Call it again after a failure to continue where the server stopped. Resolves with the finished object.
   */
  async resume(id: string, data: UploadBody, o: { chunkSize?: number; onProgress?: (p: UploadProgress) => void; signal?: AbortSignal } = {}): Promise<StorageObject> {
    const chunk = Math.max(1, o.chunkSize ?? DEFAULT_PART);
    const total = sizeOf(data);
    let offset = (await this.get(id)).received_bytes;
    for (;;) {
      const to = Math.min(total, offset + chunk);
      const r = await this.append(id, offset, slice(data, offset, to), { signal: o.signal });
      offset = r.offset;
      o.onProgress?.({ uploadedBytes: offset, totalBytes: total });
      if (r.object) return r.object;
      if (offset >= total) throw new Error(`upload ${id} received all ${total} bytes but did not complete: declare the size when creating it`);
    }
  }
}

/** Signed URLs, used without credentials: `client.storage.signed`. */
export class SignedApi {
  constructor(private readonly t: Transport) {}

  /** The absolute URL of a `SignedUrl` (or its relative path). */
  resolve(url: string | SignedUrl): string {
    const path = typeof url === "string" ? url : url.url;
    return /^https?:\/\//i.test(path) ? path : `${this.t.baseUrl}${path}`;
  }

  /** Downloads through a signed URL (no credentials sent). */
  download(url: string | SignedUrl, o: { range?: ByteRange; signal?: AbortSignal } = {}): Promise<Response> {
    return this.t.request<Response>(this.relative(url), {
      raw: true,
      anonymous: true,
      signal: o.signal,
      headers: o.range === undefined ? undefined : { Range: rangeHeader(o.range) },
    });
  }

  /** Uploads through a signed `PUT` URL (no credentials sent). */
  upload(url: string | SignedUrl, body: UploadBody, o: PutOptions = {}): Promise<StorageObject> {
    return this.t.request(this.relative(url), { method: "PUT", anonymous: true, rawBody: body as BodyInit, headers: uploadHeaders(o), signal: o.signal });
  }

  private relative(url: string | SignedUrl): string {
    const path = typeof url === "string" ? url : url.url;
    return path.startsWith(this.t.baseUrl) ? path.slice(this.t.baseUrl.length) : path;
  }
}

/** Object storage: `client.storage`. */
export class StorageApi {
  readonly buckets: BucketsApi;
  readonly uploads: UploadsApi;
  readonly signed: SignedApi;

  constructor(private readonly t: Transport) {
    this.buckets = new BucketsApi(t);
    this.uploads = new UploadsApi(t);
    this.signed = new SignedApi(t);
  }

  /** The objects of one bucket. */
  from(bucket: string): BucketApi {
    return new BucketApi(this.t, bucket);
  }

  /** Instance totals (needs the `storage_admin` permission). */
  stats(): Promise<StorageStats> {
    return this.t.request("/api/v1/storage/stats");
  }

  /** Runs the reconciler now: aborts expired uploads, removes orphaned blobs (storage admins). */
  reconcile(): Promise<ReconcileReport> {
    return this.t.request("/api/v1/storage/maintenance/reconcile", { method: "POST" });
  }

  /** Replaces the URL signing key: every signed URL issued so far stops working (administrators). */
  rotateSigningKey(): Promise<void> {
    return this.t.request("/api/v1/storage/maintenance/rotate-signing-key", { method: "POST" });
  }
}
