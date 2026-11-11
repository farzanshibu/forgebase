# ADR-0018: Object storage

- Status: accepted
- Date: 2026-11-09
- Work package: FB-30 (MDF-24). Migration `0017_storage.sql`. Crate `forgebase-storage`.
- Scope: SPEC §5 (object storage engine) as listed in PLAN FB-30: driver interface, local and S3-compatible
  drivers, streaming, multipart and resumable uploads, checksums, content-addressed deduplication, versions,
  signed URLs, bucket policies and ACLs, storage quota, staging and reconciler, REST + OpenAPI.
- Builds on: ADR-0008 (policy, `Authorizer`), ADR-0010 (`adjust_gauge`), ADR-0007 (jobs), ADR-0015 (the
  outbound HTTPS client of FB-25), ADR-0001 (one writer, error model).
- Not in this card: CORS rules, lifecycle rules, thumbnails and image hooks, server-side encryption
  (SPEC lists them under §5; follow-ups), Storage Studio (FB-33), client SDK helpers (FB-41).

## 1. Layers

| Layer | Holds |
|---|---|
| `StorageDriver` (flat key → bytes) | `put` (atomic, streaming), `get` (range, streaming), `head`, `delete`, `copy`, `rename`, `list`, `ping`. Internal keys only: `blobs/<aa>/<sha256>`, `staging/...` |
| `StorageService` | buckets, versions, checksums, dedup, quota, authorization, uploads, signed URLs, reconciler |
| `http` | `/api/v1/storage/*` as an `ApiExtension` |

Metadata lives in SQLite (tables in migration 17; versions 15 and 16 belong to other cards). Bytes never enter
the database.

## 2. Drivers

* **Local**: files under `<data dir>/storage`. A write goes to `.tmp/<id>`, is synced and renamed into place, so
  a crash or dropped connection never leaves a partial blob under its key.
* **S3-compatible**: path-style SigV4 requests (own signer, verified against the AWS-documented GET Object
  example) over `forgebase_auth::social::HttpClient`, i.e. the FB-25 rustls client for `https://` and plain
  `http://` for a loopback mock or TLS-terminating proxy. Reads page through ranged `GET`s of at most
  `read_chunk` bytes. A write of more than `part_size` bytes is a multipart upload (a failing stream aborts it);
  smaller ones one `PutObject`. S3 credentials come from `FORGEBASE_STORAGE_S3_ACCESS_KEY` /
  `..._SECRET_KEY` (never from the config file, so `config show` cannot print them).
  Limitation: the shared client buffers one response body, so a part or chunk is held in memory
  (`part_size`, `read_chunk`), not the whole object.

## 3. Write path and crash safety

Body → staging file (SHA-256, byte count and size limit run inline) → quota charge → place blob → one
transaction. Placing: if `blobs/<aa>/<sha>` exists the staged file is dropped (**deduplication**), else it is
renamed there. The transaction upserts the blob (`refcount + 1`) and installs the object version (flipping
`is_latest`). Failure points and their cleanup:

| Failure | Result |
|---|---|
| declared checksum differs, size limit hit, body ends early | staged file deleted, request fails (`422`, `413`, `400`) |
| quota refuses | staged file deleted, `429` |
| crash after placing the blob, before the transaction | orphan blob, removed by the reconciler after the grace period |
| transaction fails | quota charge reversed, orphan as above |

Metadata never points at a blob that was not placed first. Deleting the last reference sets
`zero_since`; the reconciler removes the blob after the grace period. An in-process gate keeps uploads
(read side) and blob deletion (write side) from interleaving.

Copy, move and version restore reference the same blob: no bytes move.

## 4. Versions

Per bucket `versioning`. Off: an overwrite or delete removes the old version. On: an overwrite adds a version,
a delete adds a delete marker, `?version=` reads, deletes (for good) or restores one; removing the current
version promotes the newest remaining one. At most one version per key is latest (partial unique index).

## 5. Uploads

*Multipart*: numbered parts (1-10000) in any order, replaced by re-sending; complete with an optional
part list and whole-object checksum. *Resumable*: sequential chunks with `Upload-Offset`; a chunk at another
offset is `409 offset_mismatch` with the server's offset, a dropped chunk is never recorded, and reaching the
declared size completes the upload. Parts are staged files registered in `_forgebase_storage_parts` only after
they arrived whole (and matched their checksum). Completion streams the parts through the same
hash-and-commit path. Sessions expire (`upload_ttl_secs`); only their creator (or an admin) can use them.

## 6. Authorization

Order for a caller on a bucket and key: administrators and the system; the bucket owner; the bucket
visibility (`private`, `authenticated`, `public`; reads only); ACL grants (`user:`, `service:`, `role:`,
`authenticated`, `public`, per key prefix, level read < write < admin); finally the **platform permission**
`storage_read` / `storage_write` / `storage_admin`, asked through the `Authorizer` (new `Action::Storage*`
variants in `forgebase-data`), so policy roles, the entitlement hook and the quota hook of the FB-21/FB-24
pipeline apply. Creating buckets and running the reconciler need `storage_admin`. Public visibility never
grants writes. Downloads carry `Content-Security-Policy: sandbox`; HTML, SVG and XML are served as attachments.

**FB-140** (ADR-0028 addendum FB-140): an ACL entry for a prefix covers keys under it and never the bucket
itself (bucket operations need an entry with the empty prefix); the permission step asks
`Authorizer::authorize_resource` (`engine:storage_<need>:<bucket>`, then bucket **object rules** over `key`,
`created_by`, `owner`, `owner_collection`, `size`, `content_type`, `folders`); buckets belong to a tenant or
to the instance.

## 7. Signed URLs

HMAC-SHA256 over method, bucket, key, expiry and every optional field (`version`, `download`, `ct`, `sub`),
keyed with a random 32-byte instance key created on first start (`_forgebase_storage_settings`). Verified in
constant time, then expiry: forged or altered → `403 signature_invalid`, past expiry → `403 url_expired`.
Signing needs read (GET) or write (PUT) access to the key; a signed upload bills the signer's subject.

**FB-140:** the key is sealed with the auth keyring (`signing_key_sealed`; a plain key from before is sealed
unchanged, so issued URLs keep working). New URLs (`v=2`) also sign the bucket's tenant (`t`) and the
uploader (`by`). A URL is checked when issued, never when used (rls-coverage G15, as S3 and Supabase): keep
lifetimes short (default 15 minutes); `POST /api/v1/storage/maintenance/rotate-signing-key` (administrators)
revokes every URL at once.

## 8. Quota

Feature `storage.bytes` (a `gauge`, ADR-0010) counts **logical** bytes per object version of the billing
subject (the organization the request acts in, else the caller). It is charged with `adjust_gauge` after the
body is received and before it is committed (`429 quota_exceeded`), and released when a version is deleted or
overwritten (idempotency keys `storage:<version>:add|rm|undo`). Deduplication saves disk, not quota, so a
tenant cannot learn whether another tenant stores the same bytes. Without the feature in the catalog, or for
administrators and the system (no billing subject), nothing is metered.
Limitations: an overwrite at the limit is charged before the replaced bytes are released, so it needs
headroom; in-flight staged bytes are not counted; a refused upload has already crossed the network.

## 9. Reconciler

Job `storage.reconcile` (hourly schedule `storage-reconcile`, `POST /api/v1/storage/maintenance/reconcile`).
After `reconcile_grace_secs` it removes: expired upload sessions and their parts, staged files nothing refers
to (and local `.tmp` leftovers), blobs without metadata, and unreferenced blobs. Blobs the metadata names but
the driver lost are reported (`missing_blobs`), never "repaired".

## 10. Config

`[storage]`: `driver` (`local`/`s3`), `max_object_bytes`, `upload_ttl_secs`, `reconcile_grace_secs`,
`s3_endpoint`, `s3_region`, `s3_bucket`, `s3_prefix`, `s3_part_size`.

## 11. Dependencies

No new third-party crates: `hmac`, `sha2`, `subtle`-free constant-time check through `Mac::verify_slice`,
`getrandom`, `url`, `time`, `futures-util` were already in the workspace (ADR-0002).
