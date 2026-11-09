-- 0014_identity: realm-scoped identity, phone numbers and SMS codes, account
-- lifecycle (FB-25, ADR-0015).
--
-- A realm is a separate user pool: each realm names one auth collection
-- (unique across realms), so the same email can exist once per realm and
-- every auth row keyed by collection (sessions, one-time tokens, MFA, keys,
-- provider identities, passkeys, phones) is scoped to that realm. Auth
-- collections no realm names belong to realm 'default'.
--
-- Secrets inside `config` (LDAP bind password, Turnstile secret) are sealed
-- with XChaCha20-Poly1305 under the master key, never stored in plaintext.

CREATE TABLE _forgebase_realm_identity (
    realm_id   TEXT    NOT NULL PRIMARY KEY REFERENCES _forgebase_realms (id) ON DELETE RESTRICT,
    collection TEXT    NOT NULL UNIQUE CHECK (length(collection) BETWEEN 1 AND 64),
    config     TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(config)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

INSERT INTO _forgebase_realm_identity (realm_id, collection, config, created_at, updated_at)
VALUES ('default', 'users', '{}', unixepoch(), unixepoch());

-- Verified phone numbers (E.164), one per user and unique per user pool.
CREATE TABLE _forgebase_auth_phones (
    collection  TEXT    NOT NULL,
    phone       TEXT    NOT NULL CHECK (phone GLOB '+[1-9]*' AND length(phone) BETWEEN 9 AND 16),
    user_id     TEXT    NOT NULL,
    verified_at INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (collection, phone),
    UNIQUE (collection, user_id)
) STRICT;

-- SMS one-time codes: HMAC-SHA256 under the server pepper, single use,
-- bounded attempts. `user_id` is set for codes that add a phone to a
-- signed-in account.
CREATE TABLE _forgebase_auth_sms_codes (
    code_hash  TEXT    NOT NULL PRIMARY KEY,
    purpose    TEXT    NOT NULL CHECK (purpose IN ('login', 'verify_phone')),
    collection TEXT    NOT NULL,
    phone      TEXT    NOT NULL,
    user_id    TEXT,
    detail     TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(detail)),
    attempts   INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
) STRICT;

CREATE INDEX _forgebase_auth_sms_codes_by_phone ON _forgebase_auth_sms_codes (collection, phone, purpose);

-- Account lifecycle states other than active (no row) and suspended
-- (`_forgebase_suspensions`, shared with the policy engine). A deactivated
-- account can be reactivated by an administrator; an anonymised one keeps
-- its id (so references stay valid) and can never sign in again.
CREATE TABLE _forgebase_auth_account_status (
    collection TEXT    NOT NULL,
    user_id    TEXT    NOT NULL,
    status     TEXT    NOT NULL CHECK (status IN ('deactivated', 'anonymised')),
    reason     TEXT    NOT NULL DEFAULT '',
    changed_by TEXT    NOT NULL,
    changed_at INTEGER NOT NULL,
    PRIMARY KEY (collection, user_id)
) STRICT;

-- Service accounts belong to a realm too (names stay unique platform-wide).
ALTER TABLE _forgebase_service_accounts ADD COLUMN realm TEXT NOT NULL DEFAULT 'default';

-- Realm of the account an audit event concerns (NULL: platform level).
ALTER TABLE _forgebase_auth_audit ADD COLUMN realm TEXT;
CREATE INDEX _forgebase_auth_audit_by_realm ON _forgebase_auth_audit (realm, id);
