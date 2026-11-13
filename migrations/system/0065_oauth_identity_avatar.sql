-- 0065_oauth_identity_avatar (ADR-0039): the profile picture a login provider
-- reported (an https URL, at most 2048 characters), refreshed on each sign-in.
-- Shown to the identity's owner; never used to decide anything.
ALTER TABLE _forgebase_oauth_identities ADD COLUMN avatar_url TEXT;
