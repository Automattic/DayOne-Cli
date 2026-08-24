CREATE TABLE IF NOT EXISTS comments (
  id TEXT PRIMARY KEY,
  journal_id TEXT NOT NULL,
  entry_id TEXT NOT NULL,
  author_id TEXT,
  created_at TEXT,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  content TEXT,
  data_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_comments_entry_created
  ON comments(journal_id, entry_id, COALESCE(created_at, ''), id);
CREATE INDEX IF NOT EXISTS idx_comments_entry_updated
  ON comments(journal_id, entry_id, COALESCE(updated_at, ''), id);
CREATE INDEX IF NOT EXISTS idx_comments_entry_deleted
  ON comments(journal_id, entry_id, is_deleted, deleted_at);

CREATE TABLE IF NOT EXISTS comment_reactions (
  id TEXT PRIMARY KEY,
  journal_id TEXT NOT NULL,
  entry_id TEXT NOT NULL,
  comment_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  reaction TEXT NOT NULL,
  timestamp TEXT,
  data_json TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_comment_reactions_identity
  ON comment_reactions(journal_id, entry_id, comment_id, user_id);
CREATE INDEX IF NOT EXISTS idx_comment_reactions_comment
  ON comment_reactions(journal_id, entry_id, comment_id);
