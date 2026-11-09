-- 0003_auth: authentication core (FB-20, ADR-0004).
--
-- End users live in auth collections (ADR-0003 §3; the default one is
-- `users`). These system tables hold everything else identity needs:
-- administrators, sessions and their rotating refresh tokens, one-time tokens
-- (verification, reset, magic link, email OTP, MFA challenge), TOTP factors
-- and recovery codes, service accounts, API keys and personal access tokens,
-- signing keys, lockout counters and the audit log.
--
-- Secrets are never stored in plaintext:
--   * passwords: Argon2id PHC strings;
--   * random tokens, codes and API-key secrets: HMAC-SHA256 under a server
--     pepper derived from the data directory's master key (a database leak
--     alone cannot be brute-forced, not even 6-digit codes);
--   * secrets that must be recovered (token signing keys, TOTP seeds):
--     XChaCha20-Poly1305 under a key derived from the master key.
--
-- Times are INTEGER Unix seconds (UTC), so expiry checks are plain integer
-- comparisons. Only the auth service writes these tables.

-- Administrators (superusers). Separate from end-user collections: an
-- administrator is not a record any user rule can match.
CREATE TABLE _forgebase_admins (
    id            TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    email         TEXT    NOT NULL UNIQUE COLLATE NOCASE CHECK (length(email) BETWEEN 3 AND 254),
    password_hash TEXT    NOT NULL CHECK (password_hash LIKE '$argon2id$%'),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    last_login_at INTEGER
) STRICT;

-- Token signing keys (HS256). `secret_enc` is nonce || ciphertext.
-- One key is `active` (signs); `retired` keys still verify until
-- `verify_until`, then they are deleted.
CREATE TABLE _forgebase_auth_keys (
    kid          TEXT    NOT NULL PRIMARY KEY CHECK (length(kid) BETWEEN 8 AND 64),
    alg          TEXT    NOT NULL CHECK (alg IN ('HS256')),
    secret_enc   BLOB    NOT NULL,
    status       TEXT    NOT NULL CHECK (status IN ('active', 'retired')),
    created_at   INTEGER NOT NULL,
    verify_until INTEGER
) STRICT;

CREATE UNIQUE INDEX _forgebase_auth_keys_one_active ON _forgebase_auth_keys (status) WHERE status = 'active';

-- Sessions: one per sign-in on a device. A session is also the family of its
-- refresh tokens: reusing a rotated refresh token revokes the session.
CREATE TABLE _forgebase_auth_sessions (
    id            TEXT    NOT NULL PRIMARY KEY,
    subject_type  TEXT    NOT NULL CHECK (subject_type IN ('user', 'admin')),
    collection    TEXT,
    subject_id    TEXT    NOT NULL,
    auth_method   TEXT    NOT NULL,
    mfa           INTEGER NOT NULL DEFAULT 0 CHECK (mfa IN (0, 1)),
    cookie_hash   TEXT    UNIQUE,
    ip            TEXT,
    user_agent    TEXT,
    device_name   TEXT,
    created_at    INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    revoked_at    INTEGER,
    revoked_reason TEXT,
    CHECK ((subject_type = 'user') = (collection IS NOT NULL))
) STRICT;

CREATE INDEX _forgebase_auth_sessions_by_subject ON _forgebase_auth_sessions (subject_type, collection, subject_id);

-- Rotating refresh tokens. Each use marks the token used and issues the next
-- generation; a second use of a used token is reuse (theft) and revokes the
-- whole session.
CREATE TABLE _forgebase_auth_refresh_tokens (
    token_hash  TEXT    NOT NULL PRIMARY KEY,
    session_id  TEXT    NOT NULL REFERENCES _forgebase_auth_sessions (id) ON DELETE CASCADE,
    generation  INTEGER NOT NULL CHECK (generation >= 1),
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    used_at     INTEGER
) STRICT;

CREATE INDEX _forgebase_auth_refresh_tokens_by_session ON _forgebase_auth_refresh_tokens (session_id);

-- One-time tokens and codes, delivered by mail or returned once to the
-- client (MFA challenge). Single use; attempts bound guessing of short codes.
CREATE TABLE _forgebase_auth_one_time (
    token_hash   TEXT    NOT NULL PRIMARY KEY,
    purpose      TEXT    NOT NULL CHECK (purpose IN ('verify_email', 'password_reset', 'magic_link', 'email_otp', 'mfa_challenge')),
    subject_type TEXT    NOT NULL CHECK (subject_type IN ('user', 'admin')),
    collection   TEXT,
    subject_id   TEXT    NOT NULL,
    email        TEXT,
    detail       TEXT    CHECK (detail IS NULL OR json_valid(detail)),
    attempts     INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    used_at      INTEGER
) STRICT;

CREATE INDEX _forgebase_auth_one_time_by_subject ON _forgebase_auth_one_time (purpose, subject_type, collection, subject_id);

-- TOTP factor per subject. `secret_enc` is nonce || ciphertext of the seed.
-- `last_step` blocks replay of a code inside its window.
CREATE TABLE _forgebase_auth_mfa (
    subject_type TEXT    NOT NULL CHECK (subject_type IN ('user', 'admin')),
    collection   TEXT    NOT NULL DEFAULT '',
    subject_id   TEXT    NOT NULL,
    secret_enc   BLOB    NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    last_step    INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    enabled_at   INTEGER,
    PRIMARY KEY (subject_type, collection, subject_id)
) STRICT;

CREATE TABLE _forgebase_auth_recovery_codes (
    code_hash    TEXT    NOT NULL PRIMARY KEY,
    subject_type TEXT    NOT NULL,
    collection   TEXT    NOT NULL DEFAULT '',
    subject_id   TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    used_at      INTEGER
) STRICT;

CREATE INDEX _forgebase_auth_recovery_codes_by_subject ON _forgebase_auth_recovery_codes (subject_type, collection, subject_id);

-- Machine identities. `role` = 'admin' acts as an administrator (narrowed by
-- the key's scopes); 'service' is an ordinary principal for the policy engine.
CREATE TABLE _forgebase_service_accounts (
    id          TEXT    NOT NULL PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE COLLATE NOCASE CHECK (length(name) BETWEEN 1 AND 100),
    description TEXT    NOT NULL DEFAULT '',
    role        TEXT    NOT NULL DEFAULT 'service' CHECK (role IN ('service', 'admin')),
    created_by  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    disabled_at INTEGER
) STRICT;

-- API keys (owned by service accounts) and personal access tokens (owned by
-- a user or an administrator). The token is `<prefix>_<id>_<secret>`; only
-- HMAC(pepper, secret) is kept and the full token is shown once.
CREATE TABLE _forgebase_auth_api_keys (
    id           TEXT    NOT NULL PRIMARY KEY,
    kind         TEXT    NOT NULL CHECK (kind IN ('api_key', 'pat')),
    name         TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    secret_hash  TEXT    NOT NULL,
    owner_type   TEXT    NOT NULL CHECK (owner_type IN ('service_account', 'user', 'admin')),
    collection   TEXT,
    owner_id     TEXT    NOT NULL,
    scopes       TEXT    NOT NULL CHECK (json_valid(scopes)),
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    last_used_at INTEGER,
    revoked_at   INTEGER,
    CHECK ((kind = 'api_key') = (owner_type = 'service_account'))
) STRICT;

CREATE INDEX _forgebase_auth_api_keys_by_owner ON _forgebase_auth_api_keys (owner_type, collection, owner_id);

-- Consecutive-failure counters for account lockout (survive restarts).
-- `key` is `<scope>:<normalised identifier>`, also for unknown accounts, so a
-- lockout does not reveal whether an account exists.
CREATE TABLE _forgebase_auth_lockouts (
    key          TEXT    NOT NULL PRIMARY KEY,
    failures     INTEGER NOT NULL DEFAULT 0,
    window_start INTEGER NOT NULL,
    locked_until INTEGER NOT NULL DEFAULT 0,
    lock_count   INTEGER NOT NULL DEFAULT 0
) STRICT;

-- Append-only audit log of every authentication event. `detail` never holds
-- secrets (passwords, tokens, codes); it is built from an allow list.
CREATE TABLE _forgebase_auth_audit (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    at           INTEGER NOT NULL,
    event        TEXT    NOT NULL,
    outcome      TEXT    NOT NULL CHECK (outcome IN ('success', 'failure')),
    actor        TEXT    NOT NULL,
    subject_type TEXT,
    collection   TEXT,
    subject_id   TEXT,
    ip           TEXT,
    user_agent   TEXT,
    request_id   TEXT,
    detail       TEXT    CHECK (detail IS NULL OR json_valid(detail))
) STRICT;

CREATE INDEX _forgebase_auth_audit_by_subject ON _forgebase_auth_audit (subject_type, collection, subject_id, id);
CREATE INDEX _forgebase_auth_audit_by_event ON _forgebase_auth_audit (event, id);
