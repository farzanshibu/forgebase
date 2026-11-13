-- 0066_email_providers (ADR-0039): which mail transport a realm (or, with
-- realm = '*', the instance) sends through, and the provider's message id of
-- each delivery. `config` is JSON whose secret fields are sealed under the
-- master key (`sealed:<base64url>`). Times are epoch milliseconds.
CREATE TABLE _forgebase_email_providers (
    realm      TEXT    NOT NULL PRIMARY KEY CHECK (length(realm) BETWEEN 1 AND 64),
    config     TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    updated_by TEXT
) STRICT;

ALTER TABLE _forgebase_email_deliveries ADD COLUMN provider_message_id TEXT;
