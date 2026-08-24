CREATE TABLE IF NOT EXISTS sync_outbox (
  id TEXT PRIMARY KEY,
  resource TEXT NOT NULL,
  operation TEXT NOT NULL,
  object_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  attempt_count INTEGER NOT NULL DEFAULT 0,
  next_attempt_epoch_ms INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_sync_outbox_due
  ON sync_outbox(status, next_attempt_epoch_ms, created_at);

CREATE INDEX IF NOT EXISTS idx_sync_outbox_resource_object
  ON sync_outbox(resource, object_id);

CREATE INDEX IF NOT EXISTS idx_context_library_items_needs_sync
  ON context_library_items(needs_sync);
