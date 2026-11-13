-- ADR-0039: the profile picture a login provider reported (see the SQLite
-- twin `system/0065_oauth_identity_avatar.sql`).
ALTER TABLE _forgebase_oauth_identities ADD COLUMN avatar_url text;
