-- PostgreSQL twin of migrations/system/0028_key_tenant.sql (FB-138,
-- rls-coverage G6): optional tenant binding of an API key or PAT. NULL =
-- unbound. Checked against the owner's tenancy (policy step 2) when the key
-- is created and every time it is used.

ALTER TABLE _forgebase_auth_api_keys ADD COLUMN tenant text;
