CREATE TABLE IF NOT EXISTS sync_cursors (
  resource_key TEXT NOT NULL PRIMARY KEY,
  cursor TEXT,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS sync_lock (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  locked_at_epoch_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_runs (
  id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  finished_at TEXT,
  status TEXT NOT NULL,
  fatal_error TEXT
);

CREATE TABLE IF NOT EXISTS sync_resource_runs (
  id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL,
  resource_key TEXT NOT NULL,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  finished_at TEXT,
  status TEXT NOT NULL,
  changed_count INTEGER NOT NULL DEFAULT 0,
  cursor_before TEXT,
  cursor_after TEXT,
  cursor_advanced INTEGER NOT NULL DEFAULT 0 CHECK (cursor_advanced IN (0, 1)),
  error TEXT,
  FOREIGN KEY (run_id) REFERENCES sync_runs(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS journal_sync_info (
  journal_id TEXT NOT NULL PRIMARY KEY,
  status TEXT NOT NULL,
  has_completed INTEGER NOT NULL DEFAULT 0 CHECK (has_completed IN (0, 1)),
  last_selected_at_epoch_ms INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sync_resource_state (
  resource_key TEXT NOT NULL PRIMARY KEY,
  last_successful_sync_at TEXT,
  last_error TEXT
);

CREATE TABLE IF NOT EXISTS user_profile (
  id TEXT PRIMARY KEY,
  data_json TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS user_keys (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  data_json TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS content_keys (
  fingerprint TEXT PRIMARY KEY,
  data_json TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS device_changes (
  id TEXT PRIMARY KEY,
  data_json TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS named_region_changes (
  id TEXT PRIMARY KEY,
  data_json TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS journals (
  id TEXT PRIMARY KEY,
  user_edit_date TEXT,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entries (
  id TEXT PRIMARY KEY,
  journal_id TEXT NOT NULL,
  user_edit_date TEXT,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS unread_entries (
  id TEXT PRIMARY KEY,
  journal_id TEXT,
  entry_id TEXT,
  data_json TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS templates (
  id TEXT PRIMARY KEY,
  user_edit_date TEXT,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS notifications (
  id TEXT PRIMARY KEY,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_settings (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  cursor TEXT,
  data_json TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS feature_flags (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  data_json TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS pbc_template_categories (
  id TEXT PRIMARY KEY,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pbc_templates (
  id TEXT PRIMARY KEY,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pbc_prompt_pack_gallery (
  id TEXT PRIMARY KEY,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS journal_hierarchy_items (
  id TEXT PRIMARY KEY,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS daily_chat_feed (
  id TEXT PRIMARY KEY,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS daily_chat_settings (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  updated_at TEXT,
  data_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS context_library_items (
  id TEXT PRIMARY KEY,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  needs_sync INTEGER NOT NULL DEFAULT 0 CHECK (needs_sync IN (0, 1)),
  data_json TEXT NOT NULL
);
