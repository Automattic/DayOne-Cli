-- Media files downloaded by `dayone sync --media`. One row per downloaded
-- file (the original or the thumbnail of a moment). `rel_path` is relative to
-- the profile directory so the profile can be moved without rewriting rows.
CREATE TABLE IF NOT EXISTS moment_media (
  journal_id TEXT NOT NULL,
  entry_id TEXT NOT NULL,
  moment_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('original', 'thumbnail')),
  rel_path TEXT NOT NULL,
  file_size_bytes INTEGER NOT NULL,
  -- md5 of the bytes written to disk (after any decryption).
  md5 TEXT NOT NULL,
  -- md5 the entry advertised for this file when it was downloaded; a change
  -- means the media was replaced and must be fetched again.
  source_md5 TEXT,
  downloaded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (journal_id, entry_id, moment_id, kind)
);
