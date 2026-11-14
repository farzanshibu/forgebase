# ADR-0040: Google Cloud Storage and Azure Blob drivers

- Status: accepted
- Date: 2026-11-14
- Scope: `forgebase-storage` (`gcs`, `azure`, `cloud`), `[storage]` configuration in `forgebase-core`, driver selection in
  `forgebase-server`. No migration.
- Builds on: ADR-0018 (object storage: the driver interface, the S3 driver, signed URLs), ADR-0015 (the outbound
  HTTPS client), ADR-0039 (`forgebase_auth::sigv4`).

## 1. Which targets already work, and which need a driver

The S3 driver (ADR-0018 §2) speaks path-style SigV4 over any `http(s)` endpoint, so every service with an S3 API
works with **no new code**: set `driver = "s3"`, the endpoint, the region the service documents (it is only used to
sign), the bucket and the access keys. Most deployments need nothing from this ADR.

| Target | Driver | `s3_endpoint` | `s3_region` | Notes |
|---|---|---|---|---|
| Amazon S3 | `s3` | `https://s3.<region>.amazonaws.com` | the bucket's region | Reference implementation. |
| MinIO, Ceph RGW, SeaweedFS, Garage | `s3` | the service URL | any (`us-east-1`) | Path-style is what they all accept. |
| Cloudflare R2 | `s3` | `https://<account-id>.r2.cloudflarestorage.com` | `auto` | Multipart parts must be equal-sized except the last: the driver sends equal parts. |
| Backblaze B2 | `s3` | `https://s3.<region>.backblazeb2.com` | the region in the endpoint | Parts of at least 5 MB, as S3. |
| Wasabi | `s3` | `https://s3.<region>.wasabisys.com` | the region in the endpoint | |
| DigitalOcean Spaces | `s3` | `https://<region>.digitaloceanspaces.com` | the region in the endpoint | |
| Google Cloud Storage, S3 interoperability | `s3` | `https://storage.googleapis.com` | `auto` | Needs an HMAC key of a service account (long-lived secret). Test coverage: the S3 mock runs with `region = auto`, a trailing slash on the endpoint and slashes around the prefix. |
| Google Cloud Storage, native | `gcs` | (default) | n/a | Service-account key or workload identity, no HMAC key. |
| Azure Blob Storage | `azure` | (default) | n/a | Azure has no S3 API. |
| Azurite, fake-gcs-server | `azure`, `gcs` | the emulator URL | n/a | |

The rows other than the mock-tested interoperability settings rest on the services' documentation: there is no real
cloud in the test suite (SPEC §28, ADR-0018), only protocol-level mocks. A mismatch is reported by `ping` at startup
and by the first upload.

The native drivers exist for two reasons: Google and Azure users should not have to mint long-lived HMAC keys or run
an S3 gateway, and both clouds offer **ambient identities** (GKE / GCE workload identity, Azure managed identity) that
an S3-compatible endpoint cannot use.

## 2. What stays above the driver

ADR-0018's `StorageDriver` is a flat key → bytes store (`put`, `get` with ranges, `head`, `delete`, `copy`, `list`,
`ping`). ForgeBase keeps buckets, versions, content-addressed deduplication, multipart and resumable *upload sessions*,
quotas, checksums and signed URLs itself. So the cloud drivers store **plain, unversioned objects** under internal
keys (`blobs/<aa>/<sha256>`, `staging/...`); they never read or write object generations, versions, snapshots or
metadata. Consequences:

* **Versioning** is ForgeBase's (ADR-0018 §4); enabling bucket versioning or soft delete on the cloud side only keeps
  deleted blobs longer (cost, not behaviour). The Azure driver sends `x-ms-delete-snapshots: include` so a delete never
  fails on snapshots.
* **Multipart parity** exists at the driver level, as in the S3 driver: a `put` larger than one part uses the cloud's
  own multi-step upload and is atomic: GCS **resumable upload** (a failing stream cancels the session), Azure
  **Put Block + Put Block List** (uncommitted blocks are invisible). GCS also offers `compose` (up to 32 objects,
  server side) as an inherent method.
* **Signed URLs** of the REST API stay ForgeBase's HMAC URLs (ADR-0018 §7): the S3 driver has no pre-signed URLs
  either. Both cloud drivers additionally expose `signed_url` helpers (GCS V4 `GOOG4-RSA-SHA256`, Azure service SAS)
  for operators and future offload of downloads to the store. Nothing in the REST API calls them yet.
  *Since the addendum (§10), the REST API offloads signed downloads to them, and to new S3 pre-signed URLs, under
  `[storage] signed_urls = "native" | "auto"`; the default stays ForgeBase's HMAC URLs.*

## 3. Shared pieces (`forgebase_storage::cloud`)

* **Retries**: transport errors and `408`, `429`, `500`, `502`, `503`, `504` are retried with exponential backoff
  (default 5 attempts, 250 ms doubling up to 10 s, `Retry-After` honoured). Every attempt builds, signs and sends a new
  request, so a retried Azure request carries a fresh `x-ms-date`. Other statuses are answers, not failures. A GCS
  resumable chunk that failed transiently is *not* blindly re-sent: the driver asks the session how many bytes it kept
  (`Content-Range: bytes */*`) and continues from there.
* **Token cache**: OAuth tokens are cached and replaced 120 s before they expire; concurrent callers share one refresh;
  a `401` drops the cached token and retries once with a new one.
* **Memory**: the shared HTTP client buffers one body per request (ADR-0018 §2). The drivers therefore move at most one
  upload part (`s3_part_size`) or one ranged read (4 MiB) at a time; whole objects are never held.
* **RSA**: `ring` signs RS256 JWTs and V4 URLs (`RsaKeyPair::from_pkcs8`), the same primitive ForgeBase's Google Play
  billing driver uses.
* **Clock**: drivers take an injectable clock for tests, like the S3 driver.

## 4. Google Cloud Storage (JSON API)

* **Auth** (first match; `forgebase-server`): `FORGEBASE_STORAGE_GCS_CREDENTIALS_JSON` (the key itself),
  `FORGEBASE_STORAGE_GCS_CREDENTIALS_FILE` or `GOOGLE_APPLICATION_CREDENTIALS` (a path), `FORGEBASE_STORAGE_GCS_ACCESS_TOKEN`
  (a fixed token), a custom `gcs_endpoint` with none of these (an emulator, no auth), else the metadata server
  (`GCE_METADATA_HOST` or `metadata.google.internal`) for workload identity.
  Service-account keys: RS256 JWT (`iss`, scope `devstorage.read_write`, `aud` = `token_uri`, one hour) exchanged at
  the key's `token_uri` (`urn:ietf:params:oauth:grant-type:jwt-bearer`).
* **Writes**: at most one chunk (`s3_part_size` rounded up to a multiple of 256 KiB, a GCS rule) is one
  `uploadType=media` request; more is a resumable session, one chunk per request.
* **Reads**: `GET ...?alt=media` with `Range`, at most 4 MiB per request, after a metadata `GET` for the size.
* **Copy**: `rewriteTo`, looped on `rewriteToken` until `done` (large or cross-location copies take several calls).
* **List**: `maxResults=1000` and `pageToken`; `list_with_delimiter` returns the `prefixes` as well.
* **V4 signed URLs**: `signed_url(method, key, expires)` (1 s to 7 days), signing the `host` header with
  `UNSIGNED-PAYLOAD`, path-style `https://storage.googleapis.com/<bucket>/<object>`. It needs a service-account key:
  workload identity could sign through the IAM `signBlob` API, which is not implemented. Verified against five of
  Google's published vectors (`googleapis/conformance-tests`, `v4_signatures.json`, with Google's dummy key).
  Vectors with extra signed headers, query parameters, virtual-hosted style or bucket-bound hostnames are not
  implemented.

## 5. Azure Blob Storage

* **Auth** (first match): `FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY` (SharedKey), `..._SAS_TOKEN`, Microsoft Entra client
  credentials (`..._TENANT_ID`, `..._CLIENT_ID`, `..._CLIENT_SECRET`, scope `https://storage.azure.com/.default`),
  else the **managed identity** of the host: the App Service / Container Apps endpoint when `IDENTITY_ENDPOINT` and
  `IDENTITY_HEADER` are set, otherwise IMDS (`169.254.169.254`); `..._CLIENT_ID` selects a user-assigned identity.
* **SharedKey**: HMAC-SHA256 over the documented string to sign (verb, standard headers, `x-ms-*` headers, canonical
  resource of `/<account><encoded path>` and the sorted query parameters), API version `2021-08-06`. With an account URL
  that already contains the account (`http://127.0.0.1:10000/devstoreaccount1`, Azurite) the account appears twice in
  the canonical resource, as Azure and Azurite expect.
* **Writes**: at most `s3_part_size` bytes is one `Put Blob` (block blob); more is `Put Block` per part (ids are the
  base64 of an 8-digit counter: equal length, a requirement) and one `Put Block List`.
* **Reads**: `x-ms-range` windows of at most 4 MiB. **Copy**: `Copy Blob` within the container; a `pending` copy is
  polled with `HEAD` (`x-ms-copy-status`) until it succeeds. **List**: `maxresults=1000` and `marker`;
  `list_with_delimiter` returns the `BlobPrefix` entries.
* **Signed URLs**: `signed_url(permission, key, expires)` makes a *service SAS* (`sr=b`, version `2021-08-06`) with the
  account key; the string to sign is verified against an independent computation. **User-delegation SAS** (the only
  kind Entra identities can make) is not implemented; with SAS tokens or Entra auth `signed_url` reports
  `azure_signing_unavailable`.

## 6. Configuration and secrets

`[storage] driver = "gcs" | "azure"` with `gcs_bucket`, `gcs_prefix`, `gcs_endpoint` and `azure_account`,
`azure_container`, `azure_prefix`, `azure_endpoint` (empty endpoints mean the public cloud). `s3_part_size` is the upload
chunk / block size of all three remote drivers. A selected driver without its bucket (or account and container) is a
configuration error. **Credentials are environment variables only** (listed in §4 and §5), like the S3 keys: the
config loader notes that they are set and never reads them, `config show` and the settings registry cannot print them,
and the debug output of every config and credential type omits secrets. They are not stored in the database; the master
key only seals what ForgeBase itself generates (the URL signing key).

## 7. Tests

All against loopback mock servers (`tests/support`), no network:

* a shared **conformance suite** (`support::conformance`) of one set of expectations (missing blobs, ranges, empty and
  big bodies in odd chunks, overwrite, failing streams leave nothing, copy and rename, awkward keys, listing and
  paging, deletes) runs on the local driver, the S3 mock (also with GCS-interoperability settings), the GCS mock
  (service account; no credentials), and the Azure mock (SharedKey on a normal and an Azurite-style URL, SAS, Entra);
* GCS: JWT exchange (claims, RS256 verified with the public key), refresh before expiry, `401` recovery, metadata server,
  resumable chunks including a partial write and a `503`, `compose`, paging, V4 vectors;
* Azure: Microsoft's documented string to sign and signature (recomputed independently), SharedKey recomputed from
  every request in the mock, SAS, client credentials, IMDS and App Service identity, blocks, retries with fresh
  signatures, pending copies, paging;
* each mock driver also runs the whole `StorageService` (dedup, ranges, multipart sessions, reconciler).

## 8. Dependencies

No new third-party crate. `ring` and `base64` (both already in the workspace and in the licence allow-list) are now
direct dependencies of `forgebase-storage`. The storage crate's own SigV4 copy was replaced by a re-export of
`forgebase_auth::sigv4` (ADR-0039): identical algorithm, the AWS-documented GET Object vector still passes.

## 9. Limitations

* One response or request body is buffered per call (part size or 4 MiB), not streamed (ADR-0018 §2).
* GCS signed URLs need a service-account key; Azure signed URLs need the account key; neither is used by the REST API
  (until the addendum, §10, which uses them for downloads in `native` / `auto` mode).
* Workload identity cannot sign GCS URLs, and Entra identities cannot make Azure user-delegation SAS.
* No client-side or customer-managed encryption keys, no object lock, no storage classes: bucket-level settings of the
  cloud apply unchanged.
* The Azure driver talks to one account URL and one container.
* Not exercised against the live clouds.

## 10. Addendum: native ("offloaded") signed URLs in the REST API

- Date: 2026-11-14
- Scope: `forgebase-storage` (`driver`, `s3`, `gcs`, `azure`, `signed`, `model`, `http`), `[storage] signed_urls` and
  `[storage.signed_urls_buckets]` in `forgebase-core` (with the settings registry), two fields passed by
  `forgebase-server`. No migration, no new dependency.

### 10.1 Problem and decision

A signed URL of the REST API (`POST /api/v1/storage/buckets/{bucket}/sign`, ADR-0018 §7) is a ForgeBase HMAC URL, so
every downloaded byte streams through the server even when the blobs live in S3, GCS or Azure, which can serve them
directly. Downloads may now be **offloaded**: the endpoint returns the provider's own signed URL for the blob.

| `[storage] signed_urls` | Downloads | Driver or credentials that cannot sign |
|---|---|---|
| `proxy` (default) | ForgeBase HMAC URL, unchanged | n/a |
| `native` | Provider URL | `409 native_signing_unavailable` naming the driver and the reason, after the access checks; a warning at start |
| `auto` | Provider URL when the driver can sign | ForgeBase HMAC URL |

`[storage.signed_urls_buckets]` overrides the mode per bucket: a bucket name (that name in every tenant) or
`<tenant>/<name>` (one tenant's bucket, which wins). The overrides live in the configuration, not on the bucket row,
because a bucket setting would need a migration; `FORGEBASE_STORAGE_SIGNED_URLS` sets the instance mode. The response
keeps its shape and gains `mode: "proxy" | "native"` (`serde(default)`: a client reading an older server sees `proxy`).
A proxy URL is relative to the server root; a native URL is absolute.

### 10.2 Driver interface

`StorageDriver` gains two default methods (the local driver keeps the defaults and never signs):

* `presign_support() -> Result<u64, String>`: the provider's longest validity, or why these credentials cannot sign;
* `presign_get(&PresignGet { key, now, expires_secs, content_type, content_disposition }) -> Result<String>`: an
  absolute `GET` URL for one blob, signed at `now` (the service clock, so `expires_at` is exact).

| Driver | Signs with | Maximum | Overrides | Cannot sign |
|---|---|---|---|---|
| `s3` | **New:** SigV4 query pre-signing (`s3::presigned_url`, `S3Driver::signed_url` for `GET` and `PUT`): `X-Amz-*` parameters (and `X-Amz-Security-Token` for temporary credentials) in the canonical query, `host` the only signed header, `UNSIGNED-PAYLOAD`, path-style like every request of the driver. The date, scope and encoding are `forgebase_auth::sigv4`'s; the signing-key chain is recomputed locally because the shared signer is header-only and keeps its HMAC private | 7 days | `response-content-type`, `response-content-disposition` | no access key or secret |
| `gcs` | V4 `GOOG4-RSA-SHA256` (§4), now with extra signed query parameters (`gcs::v4_signed_url_with`) | 7 days | `response-content-type`, `response-content-disposition` | workload identity (metadata server), a fixed token, no credentials |
| `azure` | Service SAS (§5), now with `rscd` / `rsct` in the string to sign (`azure::service_sas_with`) | none (ForgeBase's applies) | `rsct`, `rscd` | SAS token, Entra client credentials, managed identity, bearer token |

### 10.3 Security parity

* **Same checks, same order.** A native URL is issued only after exactly what a proxy URL needs (ADR-0018 §§6–7):
  the bucket, read access to the key (owner, visibility, ACL prefix grants, platform permission, bucket object rules),
  the version asked for, and no delete marker. The object row is read from the **primary** (a proxy URL's `head_object`
  may use a replica, harmless there because the proxy re-reads at use; a native URL is bound to the row it read). A
  refused caller gets the same `403`/`404` as before; `409 native_signing_unavailable` comes only after the checks, so it
  reveals nothing to a caller without access.
* **Scope.** The URL names one content-addressed blob (`blobs/<aa>/<sha256>` plus the driver prefix): the bytes of one
  version, and `GET` only. Changing the path, method, lifetime or an override breaks the provider's signature. It is
  bound to the version current at issuance (or `version_id`); a later overwrite does not reach it, and neither does a
  delete while the blob still exists (an older version or another object holds it, or the reconciler has not yet removed
  it). The proxy URL instead resolves the key when used.
* **Lifetime.** The requested `expires_in` (1 s to `max_signed_ttl_secs`, default 900, validated as before), capped at
  the provider's maximum; `expires_at` reports the capped value. S3 temporary credentials also end the URL when the
  session token expires.
* **Headers.** The object's `Content-Type`, and the proxy's `Content-Disposition` (`download_as`, or `attachment` for
  HTML, SVG and XML; the same `disposition()` helper), are signed overrides. The provider cannot be made to send
  `Content-Security-Policy: sandbox` or `X-Content-Type-Options: nosniff`; the content is served from the provider's
  origin, never ForgeBase's, so it cannot script ForgeBase.
* **Revocation.** `rotate-signing-key` revokes proxy URLs only. A native URL is revoked by rotating the provider
  credential (S3 access key, GCS service-account key, Azure account key).
* **Metering and audit.** Native downloads never reach ForgeBase: they are absent from its request metrics, logs and
  limits, and from any egress accounting, which therefore covers the proxy path only (the `storage.bytes` quota counts
  stored bytes, not downloads). Each native issuance is recorded in `_forgebase_auth_audit` as
  `storage.signed_url.native` (actor, request id, bucket, tenant, key, version, driver, expiry; never the URL, a bearer
  credential), best effort like the queue audit (a failure is logged, the URL still returned). Proxy issuance is
  unchanged and, as before, not audited.
* **Disclosure.** A native URL shows the provider bucket (or account and container), the prefix, the blob's SHA-256
  (already the proxy's `ETag`) and the credential's public identifier (S3 access key id, GCS service-account email, Azure
  account name), never a secret. Blobs are shared across tenants by content (ADR-0018 §8), so a holder who deletes their
  own copy and finds the URL still working learns that someone stores the same bytes (bytes they already had). Keep
  `proxy` where tenants must not learn even that.

### 10.4 Uploads stay on the proxy

Signed **uploads are never native**, in every mode: `PUT` URLs stay ForgeBase HMAC URLs and `mode` is `proxy`. A
provider upload URL would bypass what ForgeBase checks while the bytes stream through it: the bucket and instance size
limits, the allowed and declared content types (file signatures, FB-80), the checksum, bucket rules on `size` and
`content_type`, the `storage.bytes` quota, and deduplication. Worse, the driver key is the content hash, unknown until
the bytes are hashed, so the URL could only target a staging key and a second, ForgeBase-side "commit" step would have
to re-read, hash, check and copy the blob (all the cost the offload was meant to save). Provider conditions cover only
part of it (S3 POST policies and GCS `x-goog-content-length-range` bound the size; an Azure SAS cannot bound it; none
checks file signatures, rules or quota). The S3 driver can still pre-sign `PUT` (`S3Driver::signed_url("PUT", ...)`)
for operators, like the GCS and Azure helpers.

### 10.5 Tests

* `tests/native_urls.rs` (a fake signer over the local driver): default `proxy` unchanged (URL format, download,
  response keys, no audit row); native URLs name the blob of the version read, carry the type and disposition, are
  audited without the URL, and keep the REST shape with `mode: "native"`; version binding, missing versions and delete
  markers; expiry capped at the provider maximum while ForgeBase's own limits apply first; ACL, prefix grants and bucket
  visibility still refuse; uploads stay on the proxy and keep the size limit; `auto` falls back without signing
  credentials, `native` refuses clearly (also over REST, `409`) and only after the access checks; per-bucket overrides,
  the tenant entry winning.
* `tests/s3.rs`: AWS's documented pre-signed URL (*Authenticating Requests: Using Query Parameters*, signature
  `aeeed9bb...`); the mock verifies query-signed requests with code written from AWS's documentation (cross-checked against
  the same vector) and refuses a changed key, lifetime, method or override; `PUT` uploads; session tokens; lifetime
  bounds; a native download through the storage service fetched from the mock, capped at 7 days.
* `tests/gcs.rs`: Google's `Query Parameter Encoding` and `Query Parameter Ordering` vectors added to the fixture (seven
  in all); the mock serves XML API downloads after verifying the V4 signature with the service account's public key;
  forged URLs refused; a native download through the service; workload identity, fixed tokens and emulators cannot sign
  (`auto` → proxy, `native` → `409`).
* `tests/azure.rs`: a SAS vector with `rscd` / `rsct` computed with Python from Microsoft's field order (which reproduces
  the existing vector when they are empty); the mock verifies service-SAS downloads on normal and Azurite-style URLs;
  forged URLs refused; HTML served as an attachment through a native URL; SAS-token, Entra and managed-identity
  configurations cannot sign.
* `forgebase-core`: `signed_urls` default, file, environment, invalid values, per-bucket table and its key validation;
  the registry lists `storage.signed_urls` (enum, env) and `storage.signed_urls_buckets` (map, file only).

### 10.6 Limitations

* No `Content-Security-Policy` / `nosniff` on native downloads (providers cannot be told to send them).
* Native URLs are path-style (as the S3 driver's requests); virtual-hosted and bucket-bound hostnames are not
  produced, and no CDN signing (CloudFront, Cloud CDN) is implemented.
* GCS with workload identity (IAM `signBlob`) and Azure with Entra identities (user-delegation SAS) cannot sign.
* Native downloads are not visible to ForgeBase (metrics, logs, limits); only issuance is audited.
* Not exercised against the live clouds: mocks and published vectors only.
