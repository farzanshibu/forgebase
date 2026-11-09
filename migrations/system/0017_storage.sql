-- 0017_storage: object storage metadata (FB-30, ADR-0018).
--
-- Object bytes live in a storage driver (local directory or S3-compatible
-- service); these tables hold buckets, object versions, the content-addressed
-- blob registry, ACLs and upload sessions. Times are epoch milliseconds.

CREATE TABLE _forgebase_storage_buckets (
    name                  TEXT PRIMARY KEY,
    visibility            TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'authenticated', 'public')),
    versioning            INTEGER NOT NULL DEFAULT 0 CHECK (versioning IN (0, 1)),
    max_object_bytes      INTEGER CHECK (max_object_bytes IS NULL OR max_object_bytes > 0),
    allowed_content_types TEXT CHECK (allowed_content_types IS NULL OR json_valid(allowed_content_types)),
    owner                 TEXT NOT NULL,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
) STRICT;

-- Grants on a bucket or a key prefix of it. `grantee` is `user:<collection>/<id>`,
-- `service:<id>`, `role:<name>`, `authenticated` or `public`.
CREATE TABLE _forgebase_storage_acl (
    bucket     TEXT NOT NULL REFERENCES _forgebase_storage_buckets (name) ON DELETE CASCADE,
    prefix     TEXT NOT NULL DEFAULT '',
    grantee    TEXT NOT NULL,
    permission TEXT NOT NULL CHECK (permission IN ('read', 'write', 'admin')),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (bucket, prefix, grantee)
) STRICT;

-- Content-addressed blobs: one stored copy per SHA-256, counted by the object
-- versions that reference it. `zero_since` is set when the count reaches 0;
-- the reconciler deletes such blobs after a grace period.
CREATE TABLE _forgebase_storage_blobs (
    sha256     TEXT PRIMARY KEY CHECK (length(sha256) = 64),
    size       INTEGER NOT NULL CHECK (size >= 0),
    refcount   INTEGER NOT NULL DEFAULT 0 CHECK (refcount >= 0),
    created_at INTEGER NOT NULL,
    zero_since INTEGER
) STRICT;

CREATE INDEX _forgebase_storage_blobs_zero ON _forgebase_storage_blobs (zero_since) WHERE zero_since IS NOT NULL;

-- Object versions. A delete marker has no blob. At most one version per key
-- is the latest. `seq` orders versions of a key (newest has the highest).
CREATE TABLE _forgebase_storage_objects (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    id            TEXT NOT NULL UNIQUE,
    bucket        TEXT NOT NULL REFERENCES _forgebase_storage_buckets (name) ON DELETE CASCADE,
    key           TEXT NOT NULL,
    blob_sha      TEXT REFERENCES _forgebase_storage_blobs (sha256),
    size          INTEGER NOT NULL DEFAULT 0,
    content_type  TEXT NOT NULL DEFAULT 'application/octet-stream',
    metadata      TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    is_latest     INTEGER NOT NULL DEFAULT 1 CHECK (is_latest IN (0, 1)),
    delete_marker INTEGER NOT NULL DEFAULT 0 CHECK (delete_marker IN (0, 1)),
    quota_subject TEXT NOT NULL,
    created_by    TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    CHECK ((delete_marker = 1) = (blob_sha IS NULL))
) STRICT;

CREATE UNIQUE INDEX _forgebase_storage_objects_latest ON _forgebase_storage_objects (bucket, key) WHERE is_latest = 1;
CREATE INDEX _forgebase_storage_objects_key ON _forgebase_storage_objects (bucket, key, seq);
CREATE INDEX _forgebase_storage_objects_blob ON _forgebase_storage_objects (blob_sha) WHERE blob_sha IS NOT NULL;

-- Multipart and resumable upload sessions.
CREATE TABLE _forgebase_storage_uploads (
    id             TEXT PRIMARY KEY,
    bucket         TEXT NOT NULL REFERENCES _forgebase_storage_buckets (name) ON DELETE CASCADE,
    key            TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('multipart', 'resumable')),
    content_type   TEXT NOT NULL DEFAULT 'application/octet-stream',
    metadata       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    declared_size  INTEGER,
    declared_sha   TEXT,
    received_bytes INTEGER NOT NULL DEFAULT 0,
    next_part      INTEGER NOT NULL DEFAULT 1,
    created_by     TEXT NOT NULL,
    quota_subject  TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL
) STRICT;

CREATE INDEX _forgebase_storage_uploads_expiry ON _forgebase_storage_uploads (expires_at);

CREATE TABLE _forgebase_storage_parts (
    upload_id   TEXT NOT NULL REFERENCES _forgebase_storage_uploads (id) ON DELETE CASCADE,
    part_number INTEGER NOT NULL CHECK (part_number BETWEEN 1 AND 10000),
    size        INTEGER NOT NULL,
    sha256      TEXT NOT NULL,
    staging_key TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (upload_id, part_number)
) STRICT;

CREATE TABLE _forgebase_storage_settings (
    name  TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
