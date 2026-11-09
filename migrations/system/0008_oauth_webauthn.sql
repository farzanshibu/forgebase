-- 0008_oauth_webauthn: social login and passkeys (FB-22, ADR-0009).
--
-- `realm` is carried on every row (default 'default') so provider identities
-- and credentials can be isolated per realm when FB-25 introduces realm-scoped
-- identity; nothing here assumes a single realm.
--
-- Secrets: OAuth state tokens are stored as HMAC-SHA256 under the server pepper;
-- the PKCE verifier is sealed with XChaCha20-Poly1305 (bound to the state row).
-- WebAuthn public keys and challenges are not secret.

-- A pending authorization request. Single use: `used_at` is claimed with
-- `UPDATE ... WHERE used_at IS NULL`, so a replayed callback cannot win twice.
CREATE TABLE _forgebase_oauth_states (
    state_hash     TEXT    NOT NULL PRIMARY KEY,
    realm          TEXT    NOT NULL DEFAULT 'default',
    provider       TEXT    NOT NULL CHECK (length(provider) BETWEEN 1 AND 64),
    purpose        TEXT    NOT NULL CHECK (purpose IN ('login', 'link')),
    nonce          TEXT    NOT NULL,
    verifier_enc   BLOB,
    redirect_uri   TEXT    NOT NULL,
    collection     TEXT    NOT NULL,
    link_user_id   TEXT,
    options        TEXT    NOT NULL DEFAULT '{}',
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL,
    used_at        INTEGER
) STRICT;

CREATE INDEX _forgebase_oauth_states_expiry ON _forgebase_oauth_states (expires_at);

-- A provider account linked to a user. (realm, provider, provider_subject) is
-- unique: one provider account can belong to one user only.
CREATE TABLE _forgebase_oauth_identities (
    id               TEXT    NOT NULL PRIMARY KEY,
    realm            TEXT    NOT NULL DEFAULT 'default',
    provider         TEXT    NOT NULL,
    provider_subject TEXT    NOT NULL CHECK (length(provider_subject) BETWEEN 1 AND 255),
    collection       TEXT    NOT NULL,
    user_id          TEXT    NOT NULL,
    email            TEXT,
    email_verified   INTEGER NOT NULL DEFAULT 0 CHECK (email_verified IN (0, 1)),
    display_name     TEXT,
    created_at       INTEGER NOT NULL,
    last_login_at    INTEGER,
    UNIQUE (realm, provider, provider_subject)
) STRICT;

CREATE INDEX _forgebase_oauth_identities_by_user ON _forgebase_oauth_identities (collection, user_id);

-- Passkeys. `credential_id` is base64url. `public_key` is the COSE key.
-- `sign_count` only moves forward (clone detection).
CREATE TABLE _forgebase_webauthn_credentials (
    credential_id   TEXT    NOT NULL PRIMARY KEY CHECK (length(credential_id) BETWEEN 1 AND 1400),
    realm           TEXT    NOT NULL DEFAULT 'default',
    collection      TEXT    NOT NULL,
    user_id         TEXT    NOT NULL,
    user_handle     TEXT    NOT NULL,
    public_key      BLOB    NOT NULL,
    alg             INTEGER NOT NULL,
    sign_count      INTEGER NOT NULL DEFAULT 0,
    uv              INTEGER NOT NULL DEFAULT 0 CHECK (uv IN (0, 1)),
    backup_eligible INTEGER NOT NULL DEFAULT 0 CHECK (backup_eligible IN (0, 1)),
    backup_state    INTEGER NOT NULL DEFAULT 0 CHECK (backup_state IN (0, 1)),
    transports      TEXT,
    name            TEXT,
    use_as_mfa      INTEGER NOT NULL DEFAULT 0 CHECK (use_as_mfa IN (0, 1)),
    created_at      INTEGER NOT NULL,
    last_used_at    INTEGER
) STRICT;

CREATE INDEX _forgebase_webauthn_credentials_by_user ON _forgebase_webauthn_credentials (collection, user_id);

-- Ceremony challenges: single use, short lived, stored as HMAC of the random
-- ceremony id handed to the client.
CREATE TABLE _forgebase_webauthn_challenges (
    ceremony_hash TEXT    NOT NULL PRIMARY KEY,
    purpose       TEXT    NOT NULL CHECK (purpose IN ('register', 'login', 'mfa')),
    challenge     TEXT    NOT NULL,
    collection    TEXT,
    user_id       TEXT,
    detail        TEXT    NOT NULL DEFAULT '{}',
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    used_at       INTEGER
) STRICT;
