ALTER TABLE unread_entries ADD COLUMN deleted_at TEXT;
ALTER TABLE unread_entries ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1));
