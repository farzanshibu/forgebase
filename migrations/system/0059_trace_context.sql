-- ADR-0035: trace context. A job remembers the W3C `traceparent` of the span
-- that enqueued it (its run continues that trace); a function execution log
-- row keeps the trace id of the run.
ALTER TABLE _forgebase_jobs ADD COLUMN traceparent TEXT;
ALTER TABLE _forgebase_function_logs ADD COLUMN trace_id TEXT;
