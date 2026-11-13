-- ADR-0039: mail transport per realm and the provider's message id (see the
-- SQLite twin `system/0066_email_providers.sql`).
CREATE TABLE _forgebase_email_providers (
    realm      text   NOT NULL PRIMARY KEY CHECK (length(realm) BETWEEN 1 AND 64),
    config     text   NOT NULL,
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    updated_by text
);

ALTER TABLE _forgebase_email_deliveries ADD COLUMN provider_message_id text;
