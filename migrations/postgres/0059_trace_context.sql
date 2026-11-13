-- ADR-0035: trace context (see the SQLite twin `system/0059_trace_context.sql`).
ALTER TABLE _forgebase_jobs ADD COLUMN traceparent text;
ALTER TABLE _forgebase_function_logs ADD COLUMN trace_id text;
