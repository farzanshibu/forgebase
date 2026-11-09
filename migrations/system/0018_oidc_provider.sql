-- 0018_oidc_provider: ForgeBase as identity provider (FB-26, ADR-0019).
--
-- Client Apps, an OAuth 2.0 authorization server (authorization code + PKCE,
-- client credentials, refresh tokens with rotation, device authorization),
-- OpenID Connect (ES256 signing keys per realm, ID tokens, userinfo), consent
-- and cross-app single sign-on inside a realm.
--
-- Secrets: client secrets, authorization codes, refresh tokens, device codes
-- and user codes are stored as HMAC-SHA256 under the server pepper; signing
-- keys (P-256 private scalars) are sealed with XChaCha20-Poly1305 under the
-- master key (associated data = 'oidc:' || kid). Nothing here is plaintext.

-- A Client App of a realm. `id` is the public client_id.
CREATE TABLE _forgebase_oauth_clients (
    id                          TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 8 AND 64),
    realm                       TEXT    NOT NULL REFERENCES _forgebase_realms (id) ON DELETE RESTRICT,
    name                        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description                 TEXT,
    client_type                 TEXT    NOT NULL CHECK (client_type IN ('confidential', 'public')),
    secret_hash                 TEXT,
    secret_created_at           INTEGER,
    previous_secret_hash        TEXT,
    previous_secret_expires_at  INTEGER,
    redirect_uris               TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(redirect_uris)),
    post_logout_redirect_uris   TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(post_logout_redirect_uris)),
    grant_types                 TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(grant_types)),
    scopes                      TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(scopes)),
    skip_consent                INTEGER NOT NULL DEFAULT 0 CHECK (skip_consent IN (0, 1)),
    access_token_ttl            INTEGER,
    refresh_token_ttl           INTEGER,
    disabled                    INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1)),
    created_by                  TEXT,
    created_at                  INTEGER NOT NULL,
    updated_at                  INTEGER NOT NULL,
    CHECK ((client_type = 'confidential') = (secret_hash IS NOT NULL))
) STRICT;

CREATE INDEX _forgebase_oauth_clients_by_realm ON _forgebase_oauth_clients (realm, created_at);

-- ES256 signing keys for ID tokens and access tokens, one active per realm.
-- Retired keys stay in the JWKS until `verify_until`, then are deleted.
CREATE TABLE _forgebase_oidc_keys (
    kid          TEXT    NOT NULL PRIMARY KEY CHECK (length(kid) BETWEEN 8 AND 64),
    realm        TEXT    NOT NULL,
    alg          TEXT    NOT NULL CHECK (alg IN ('ES256')),
    private_enc  BLOB    NOT NULL,
    public_x     TEXT    NOT NULL,
    public_y     TEXT    NOT NULL,
    status       TEXT    NOT NULL CHECK (status IN ('active', 'retired')),
    created_at   INTEGER NOT NULL,
    verify_until INTEGER
) STRICT;

CREATE UNIQUE INDEX _forgebase_oidc_keys_one_active ON _forgebase_oidc_keys (realm) WHERE status = 'active';

-- An authorization request waiting for sign-in or consent (10 minutes).
CREATE TABLE _forgebase_oauth_requests (
    id          TEXT    NOT NULL PRIMARY KEY,
    realm       TEXT    NOT NULL,
    client_id   TEXT    NOT NULL REFERENCES _forgebase_oauth_clients (id) ON DELETE CASCADE,
    params      TEXT    NOT NULL CHECK (json_valid(params)),
    session_id  TEXT,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    used_at     INTEGER
) STRICT;

CREATE INDEX _forgebase_oauth_requests_expiry ON _forgebase_oauth_requests (expires_at);

-- Remembered consent: the scopes a user granted a client. Revocable; revoking
-- also revokes every token family of that client and user.
CREATE TABLE _forgebase_oauth_consents (
    id          TEXT    NOT NULL PRIMARY KEY,
    realm       TEXT    NOT NULL,
    client_id   TEXT    NOT NULL REFERENCES _forgebase_oauth_clients (id) ON DELETE CASCADE,
    collection  TEXT    NOT NULL,
    user_id     TEXT    NOT NULL,
    scopes      TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(scopes)),
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    UNIQUE (client_id, collection, user_id)
) STRICT;

CREATE INDEX _forgebase_oauth_consents_by_user ON _forgebase_oauth_consents (collection, user_id);

-- A token family: everything issued from one grant (an authorization code, a
-- device approval, or one client-credentials request). Revoking the family
-- kills its refresh tokens and makes its access tokens inactive.
CREATE TABLE _forgebase_oauth_families (
    id             TEXT    NOT NULL PRIMARY KEY,
    realm          TEXT    NOT NULL,
    client_id      TEXT    NOT NULL REFERENCES _forgebase_oauth_clients (id) ON DELETE CASCADE,
    grant_type     TEXT    NOT NULL,
    collection     TEXT,
    user_id        TEXT,
    scopes         TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(scopes)),
    session_id     TEXT,
    auth_time      INTEGER,
    acr            TEXT,
    amr            TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(amr)),
    impersonator   TEXT,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL,
    revoked_at     INTEGER,
    revoked_reason TEXT,
    CHECK ((collection IS NULL) = (user_id IS NULL))
) STRICT;

CREATE INDEX _forgebase_oauth_families_by_user ON _forgebase_oauth_families (collection, user_id, client_id);
CREATE INDEX _forgebase_oauth_families_by_session ON _forgebase_oauth_families (session_id) WHERE session_id IS NOT NULL;

-- Authorization codes: single use (`used_at` claimed with
-- `UPDATE ... WHERE used_at IS NULL`); `family_id` is set on redemption so a
-- replay can revoke what the first redemption issued.
CREATE TABLE _forgebase_oauth_codes (
    code_hash       TEXT    NOT NULL PRIMARY KEY,
    realm           TEXT    NOT NULL,
    client_id       TEXT    NOT NULL REFERENCES _forgebase_oauth_clients (id) ON DELETE CASCADE,
    redirect_uri    TEXT    NOT NULL,
    scopes          TEXT    NOT NULL CHECK (json_valid(scopes)),
    nonce           TEXT,
    code_challenge  TEXT,
    collection      TEXT    NOT NULL,
    user_id         TEXT    NOT NULL,
    session_id      TEXT    NOT NULL,
    auth_time       INTEGER NOT NULL,
    acr             TEXT    NOT NULL,
    amr             TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(amr)),
    impersonator    TEXT,
    created_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    used_at         INTEGER,
    family_id       TEXT
) STRICT;

CREATE INDEX _forgebase_oauth_codes_expiry ON _forgebase_oauth_codes (expires_at);

-- Rotating refresh tokens of a family; a second use of a used token revokes
-- the family (theft detection, as for sessions in ADR-0004 §3).
CREATE TABLE _forgebase_oauth_refresh_tokens (
    token_hash  TEXT    NOT NULL PRIMARY KEY,
    family_id   TEXT    NOT NULL REFERENCES _forgebase_oauth_families (id) ON DELETE CASCADE,
    generation  INTEGER NOT NULL CHECK (generation >= 1),
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    used_at     INTEGER
) STRICT;

CREATE INDEX _forgebase_oauth_refresh_tokens_by_family ON _forgebase_oauth_refresh_tokens (family_id);

-- Access tokens revoked one by one (RFC 7009), kept until they expire.
CREATE TABLE _forgebase_oauth_revoked_tokens (
    jti        TEXT    NOT NULL PRIMARY KEY,
    family_id  TEXT    NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;

-- Device authorization (RFC 8628). `user_code_hash` is the HMAC of the
-- normalised user code (upper case, no separators).
CREATE TABLE _forgebase_oauth_device_codes (
    device_code_hash TEXT    NOT NULL PRIMARY KEY,
    user_code_hash   TEXT    NOT NULL UNIQUE,
    realm            TEXT    NOT NULL,
    client_id        TEXT    NOT NULL REFERENCES _forgebase_oauth_clients (id) ON DELETE CASCADE,
    scopes           TEXT    NOT NULL CHECK (json_valid(scopes)),
    status           TEXT    NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied', 'used')),
    collection       TEXT,
    user_id          TEXT,
    session_id       TEXT,
    auth_time        INTEGER,
    acr              TEXT,
    amr              TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(amr)),
    interval_secs    INTEGER NOT NULL,
    last_poll_at     INTEGER,
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    family_id        TEXT
) STRICT;

CREATE INDEX _forgebase_oauth_device_codes_expiry ON _forgebase_oauth_device_codes (expires_at);
