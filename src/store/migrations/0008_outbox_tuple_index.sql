CREATE INDEX IF NOT EXISTS idx_sync_outbox_resource_operation_object_status_updated
  ON sync_outbox(resource, operation, object_id, status, updated_at);
