-- FIX-9 (ADR-0025): pull claims the oldest ready deliveries of a
-- subscription. The (subscription, state, available_at) index cannot give
-- sequence order, so SQLite sorted every ready row on each pull, a cost that
-- grew with the backlog. This index returns ready rows already in `seq` order,
-- so a pull reads only as many rows as it claims.
CREATE INDEX _forgebase_queue_deliveries_order ON _forgebase_queue_deliveries (subscription, state, seq);
