-- 0029_storage_rls: object storage tenancy and object rules (FB-140,
-- rls-coverage G14).
--
-- * `_forgebase_storage_buckets.tenant`: NULL for an instance-wide bucket
--   (every bucket before FB-140), else the tenant that owns it. A tenant
--   bucket is stored under the name `<tenant>/<name>`; bucket names never
--   hold '/', so names are unique per tenant and never meet an
--   instance-wide name.
-- * `owner`, `owner_collection`, `folders` on object versions: the item
--   fields of bucket resource rules (ADR-0028 addendum FB-140), kept as
--   columns so a rule compiles into the object SQL. `owner` is the creator's
--   id as `@request.auth.id` reports it, `owner_collection` a user
--   creator's auth collection, `folders` the JSON array of the key's folder
--   names (`a/b/c.txt` -> ["a","b"]). Existing rows are filled below with
--   exactly the values the service writes.

ALTER TABLE _forgebase_storage_buckets ADD COLUMN tenant TEXT;
ALTER TABLE _forgebase_storage_objects ADD COLUMN owner TEXT;
ALTER TABLE _forgebase_storage_objects ADD COLUMN owner_collection TEXT;
ALTER TABLE _forgebase_storage_objects ADD COLUMN folders TEXT CHECK (folders IS NULL OR json_valid(folders));

UPDATE _forgebase_storage_objects SET
    owner = CASE
        WHEN created_by LIKE 'user:%/%' THEN substr(created_by, instr(created_by, '/') + 1)
        WHEN created_by LIKE 'service:%' THEN substr(created_by, 9)
        WHEN created_by LIKE 'admin:%' THEN substr(created_by, 7)
    END,
    owner_collection = CASE
        WHEN created_by LIKE 'user:%/%' THEN substr(created_by, 6, instr(created_by, '/') - 6)
    END,
    folders = json_remove(
        '["' || replace(replace(replace(key, '\', '\\'), '"', '\"'), '/', '","') || '"]',
        '$[#-1]'
    );

CREATE INDEX _forgebase_storage_buckets_tenant ON _forgebase_storage_buckets (tenant) WHERE tenant IS NOT NULL;
CREATE INDEX _forgebase_storage_objects_owner ON _forgebase_storage_objects (bucket, owner) WHERE owner IS NOT NULL;
