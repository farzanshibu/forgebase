-- FB-138 (rls-coverage G6): optional tenant binding of an API key or PAT.
-- NULL = unbound (the request selects a tenant, as before). A bound key acts
-- in that tenant only: the HTTP layer selects it (and refuses another one),
-- and RESP sessions run in it. The binding is checked against the owner's
-- tenancy (policy step 2) when the key is created and every time it is used.
ALTER TABLE _forgebase_auth_api_keys ADD COLUMN tenant TEXT;
