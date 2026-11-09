-- FB-23 (ADR-0014): admin impersonation.
--
-- An impersonation is an ordinary user session whose `impersonator` names the
-- administrator who started it (and `impersonation_reason` why). Every audit
-- row written while one is active records the administrator.

ALTER TABLE _forgebase_auth_sessions ADD COLUMN impersonator TEXT;
ALTER TABLE _forgebase_auth_sessions ADD COLUMN impersonation_reason TEXT;
CREATE INDEX _forgebase_auth_sessions_by_impersonator
    ON _forgebase_auth_sessions (impersonator, created_at) WHERE impersonator IS NOT NULL;

ALTER TABLE _forgebase_auth_audit ADD COLUMN impersonator TEXT;
ALTER TABLE _forgebase_authz_audit ADD COLUMN impersonator TEXT;
