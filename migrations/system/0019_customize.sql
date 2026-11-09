-- FB-74 (ADR-0020): product customization. Per-realm custom domains,
-- white-label branding, transactional email (templates, delivery log) and
-- versioned legal agreements with acceptance records. Times are epoch
-- milliseconds. `realm` is a realm id (`default` for the pre-realm data).

-- A custom domain of a realm. `status` moves pending -> verified (the DNS TXT
-- record `_forgebase-verify.<host>` carries `token`); a pending domain whose
-- `expires_at` passed must be re-issued. Only verified domains route requests.
CREATE TABLE _forgebase_domains (
    id           TEXT    NOT NULL PRIMARY KEY,
    realm        TEXT    NOT NULL,
    host         TEXT    NOT NULL UNIQUE CHECK (length(host) BETWEEN 3 AND 253 AND host = lower(host)),
    token        TEXT    NOT NULL,
    status       TEXT    NOT NULL CHECK (status IN ('pending', 'verified')),
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    verified_at  INTEGER,
    last_checked INTEGER,
    last_error   TEXT,
    created_by   TEXT    NOT NULL
) STRICT;
CREATE INDEX _forgebase_domains_realm ON _forgebase_domains (realm);

-- White-label branding of a realm: one row per realm.
CREATE TABLE _forgebase_branding (
    realm        TEXT    NOT NULL PRIMARY KEY,
    display_name TEXT,
    -- Colours as #rgb / #rrggbb.
    primary_color    TEXT,
    background_color TEXT,
    text_color       TEXT,
    accent_color     TEXT,
    -- Sign-in page text (plain text, escaped when rendered).
    signin_title    TEXT,
    signin_subtitle TEXT,
    signin_footer   TEXT,
    support_email   TEXT,
    updated_at   INTEGER NOT NULL,
    updated_by   TEXT    NOT NULL
) STRICT;

-- Uploaded images of a realm: kind `logo` or `favicon`.
CREATE TABLE _forgebase_branding_assets (
    realm        TEXT    NOT NULL,
    kind         TEXT    NOT NULL CHECK (kind IN ('logo', 'favicon')),
    content_type TEXT    NOT NULL,
    bytes        BLOB    NOT NULL,
    etag         TEXT    NOT NULL,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (realm, kind)
) STRICT;

-- Email templates per realm. A realm without a row for `key` uses the
-- built-in template of that key.
CREATE TABLE _forgebase_email_templates (
    realm      TEXT    NOT NULL,
    key        TEXT    NOT NULL CHECK (length(key) BETWEEN 1 AND 64),
    subject    TEXT    NOT NULL,
    text_body  TEXT    NOT NULL,
    html_body  TEXT,
    updated_at INTEGER NOT NULL,
    updated_by TEXT    NOT NULL,
    PRIMARY KEY (realm, key)
) STRICT;

-- Delivery log of transactional email. The rendered body is kept only while
-- the message is queued or retrying (it may carry a reset link) and cleared
-- once it is sent or has failed for good; the API never returns it.
CREATE TABLE _forgebase_email_deliveries (
    id          TEXT    NOT NULL PRIMARY KEY,
    realm       TEXT    NOT NULL,
    template    TEXT,
    recipient   TEXT    NOT NULL,
    subject     TEXT    NOT NULL,
    status      TEXT    NOT NULL CHECK (status IN ('queued', 'retrying', 'sent', 'failed')),
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT,
    provider    TEXT,
    job_id      TEXT,
    text_body   TEXT,
    html_body   TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    sent_at     INTEGER
) STRICT;
CREATE INDEX _forgebase_email_deliveries_realm ON _forgebase_email_deliveries (realm, created_at);
CREATE INDEX _forgebase_email_deliveries_status ON _forgebase_email_deliveries (status);

-- Versions of a legal agreement (`kind`: terms, privacy, or another key) per
-- realm. A published version is immutable; the highest published version of
-- a kind is the current one and must be accepted by every user of the realm.
CREATE TABLE _forgebase_agreements (
    id           TEXT    NOT NULL PRIMARY KEY,
    realm        TEXT    NOT NULL,
    kind         TEXT    NOT NULL CHECK (length(kind) BETWEEN 1 AND 64),
    version      INTEGER NOT NULL CHECK (version >= 1),
    title        TEXT    NOT NULL,
    body         TEXT    NOT NULL,
    url          TEXT,
    published_at INTEGER NOT NULL,
    published_by TEXT    NOT NULL,
    UNIQUE (realm, kind, version)
) STRICT;

CREATE TRIGGER _forgebase_agreements_immutable_update BEFORE UPDATE ON _forgebase_agreements
BEGIN SELECT RAISE(ABORT, 'agreement versions are immutable'); END;
CREATE TRIGGER _forgebase_agreements_immutable_delete BEFORE DELETE ON _forgebase_agreements
BEGIN SELECT RAISE(ABORT, 'agreement versions are immutable'); END;

-- Acceptance records: who accepted which version, when, from where. Append only.
CREATE TABLE _forgebase_agreement_acceptances (
    id          TEXT    NOT NULL PRIMARY KEY,
    realm       TEXT    NOT NULL,
    principal   TEXT    NOT NULL,
    kind        TEXT    NOT NULL,
    version     INTEGER NOT NULL,
    accepted_at INTEGER NOT NULL,
    ip          TEXT,
    user_agent  TEXT,
    UNIQUE (realm, principal, kind, version)
) STRICT;
CREATE INDEX _forgebase_agreement_acceptances_principal ON _forgebase_agreement_acceptances (realm, principal);

CREATE TRIGGER _forgebase_acceptances_immutable_update BEFORE UPDATE ON _forgebase_agreement_acceptances
BEGIN SELECT RAISE(ABORT, 'acceptance records are append only'); END;
CREATE TRIGGER _forgebase_acceptances_immutable_delete BEFORE DELETE ON _forgebase_agreement_acceptances
BEGIN SELECT RAISE(ABORT, 'acceptance records are append only'); END;
