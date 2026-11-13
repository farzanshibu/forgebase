-- ADR-0036: audit investigation. The auth audit log is searched by actor,
-- client address, request id and time range, and the usage events by time.
CREATE INDEX _forgebase_auth_audit_by_actor ON _forgebase_auth_audit (actor, id);
CREATE INDEX _forgebase_auth_audit_by_ip ON _forgebase_auth_audit (ip, id);
CREATE INDEX _forgebase_auth_audit_by_request ON _forgebase_auth_audit (request_id);
CREATE INDEX _forgebase_auth_audit_by_at ON _forgebase_auth_audit (at, id);
CREATE INDEX _forgebase_usage_events_at ON _forgebase_usage_events (at);
