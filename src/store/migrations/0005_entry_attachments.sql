CREATE TABLE IF NOT EXISTS entry_attachments (
  journal_id TEXT NOT NULL,
  entry_id TEXT NOT NULL,
  moment_id TEXT NOT NULL,
  file_path TEXT NOT NULL,
  moment_type TEXT NOT NULL,
  content_type TEXT NOT NULL,
  file_size_bytes INTEGER NOT NULL,
  md5_body TEXT NOT NULL,
  width INTEGER,
  height INTEGER,
  duration_seconds REAL,
  thumbnail_content_type TEXT,
  thumbnail_md5 TEXT,
  thumbnail_width INTEGER,
  thumbnail_height INTEGER,
  thumbnail_file_size_bytes INTEGER,
  thumbnail_bytes BLOB,
  entry_associated INTEGER NOT NULL DEFAULT 0 CHECK (entry_associated IN (0, 1)),
  original_uploaded INTEGER NOT NULL DEFAULT 0 CHECK (original_uploaded IN (0, 1)),
  last_error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (journal_id, entry_id, moment_id)
);

CREATE INDEX IF NOT EXISTS idx_entry_attachments_pending_original
  ON entry_attachments(original_uploaded, entry_associated, created_at);
