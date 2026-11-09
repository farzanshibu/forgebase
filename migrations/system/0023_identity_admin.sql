-- FIX-4 (MDF-59): identity settings managed over REST instead of files.
--
-- * Social / OIDC login providers per realm (replaces editing
--   <data_dir>/auth/oauth-providers.json, which is still read at boot).
-- * SMS gateway per realm (replaces <data_dir>/auth/sms.json, still read).
--
-- Secrets (client secret, Apple key, gateway token) are write-only over the
-- API and sealed under the master key at rest ("sealed:<base64url>").

CREATE TABLE _forgebase_oauth_providers (
    realm      TEXT    NOT NULL REFERENCES _forgebase_realms (id) ON DELETE CASCADE,
    id         TEXT    NOT NULL CHECK (length(id) BETWEEN 1 AND 64),
    config     TEXT    NOT NULL CHECK (json_valid(config)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (realm, id)
) STRICT;

CREATE TABLE _forgebase_sms_config (
    realm      TEXT    NOT NULL PRIMARY KEY REFERENCES _forgebase_realms (id) ON DELETE CASCADE,
    config     TEXT    NOT NULL CHECK (json_valid(config)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
