-- Durable, store-and-forward queue for product analytics (Automattic Tracks).
--
-- Events are enqueued by command handlers and drained (POSTed in a batch to
-- the Tracks REST endpoint) on a best-effort background flush at the end of
-- each invocation and piggybacked on `dayone sync`. Rows are deleted once
-- successfully sent.
CREATE TABLE IF NOT EXISTS analytics_events (
  id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
  event_name TEXT NOT NULL,
  params_json TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_analytics_events_created_at
  ON analytics_events (created_at_ms);
