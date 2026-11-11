-- PostgreSQL twin of migrations/system/0029_storage_rls.sql (FB-140,
-- rls-coverage G14): bucket tenancy and the item fields of object rules.
-- A tenant bucket is stored under the name `<tenant>/<name>`; `owner` is the
-- creator's id as `@request.auth.id` reports it, `owner_collection` a user
-- creator's auth collection, `folders` the key's folder names
-- (`a/b/c.txt` -> ["a","b"]). Existing rows are filled with exactly the
-- values the service writes.

ALTER TABLE _forgebase_storage_buckets ADD COLUMN tenant text;
ALTER TABLE _forgebase_storage_objects ADD COLUMN owner text;
ALTER TABLE _forgebase_storage_objects ADD COLUMN owner_collection text;
ALTER TABLE _forgebase_storage_objects ADD COLUMN folders jsonb;

UPDATE _forgebase_storage_objects SET
    owner = CASE
        WHEN created_by LIKE 'user:%/%' THEN substr(created_by, strpos(created_by, '/') + 1)
        WHEN created_by LIKE 'service:%' THEN substr(created_by, 9)
        WHEN created_by LIKE 'admin:%' THEN substr(created_by, 7)
    END,
    owner_collection = CASE
        WHEN created_by LIKE 'user:%/%' THEN substr(created_by, 6, strpos(created_by, '/') - 6)
    END,
    folders = to_jsonb(trim_array(string_to_array(key, '/'), 1));

CREATE INDEX _forgebase_storage_buckets_tenant ON _forgebase_storage_buckets (tenant) WHERE tenant IS NOT NULL;
CREATE INDEX _forgebase_storage_objects_owner ON _forgebase_storage_objects (bucket, owner) WHERE owner IS NOT NULL;
