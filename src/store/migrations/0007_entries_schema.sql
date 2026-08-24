-- Migration: Move entries to full SQL schema
-- This migration extracts commonly accessed fields from the JSON blob into proper columns
-- for better query performance and simpler SQL queries.

-- Create new entries table with full schema
CREATE TABLE IF NOT EXISTS entries_new (
  id TEXT PRIMARY KEY,
  journal_id TEXT NOT NULL,
  
  -- Timestamps
  date REAL,
  user_edit_date TEXT,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  
  -- Content fields
  title TEXT,
  body TEXT,
  decrypted_text TEXT,
  rich_text_json TEXT,
  time_zone TEXT,
  
  -- Metadata (stored as JSON for flexibility)
  tags_json TEXT,
  moments_json TEXT,
  client_meta_json TEXT,
  extra_fields_json TEXT,
  
  -- Keep remaining fields in JSON for flexibility
  data_json TEXT NOT NULL
);

-- Migrate data from old table to new table
-- Extract commonly accessed fields from JSON
INSERT INTO entries_new (
  id,
  journal_id,
  date,
  user_edit_date,
  updated_at,
  deleted_at,
  is_deleted,
  title,
  body,
  decrypted_text,
  rich_text_json,
  time_zone,
  tags_json,
  moments_json,
  client_meta_json,
  extra_fields_json,
  data_json
)
SELECT
  id,
  journal_id,
  -- Extract fields from root (non-null) or payload fallback.
  -- COALESCE alone is wrong here: SQLite's json_extract returns SQL NULL for
  -- JSON null, so COALESCE falls through.  We need json_type to distinguish
  -- "path missing" (NULL) from "path is JSON null" ('null').
  CASE WHEN json_type(data_json, '$.date') IS NOT NULL
            AND json_type(data_json, '$.date') != 'null'
       THEN json_extract(data_json, '$.date')
       ELSE json_extract(data_json, '$.payload.date')
  END AS date,
  user_edit_date,
  updated_at,
  deleted_at,
  is_deleted,
  CASE WHEN json_type(data_json, '$.title') IS NOT NULL
            AND json_type(data_json, '$.title') != 'null'
       THEN json_extract(data_json, '$.title')
       ELSE json_extract(data_json, '$.payload.title')
  END AS title,
  CASE WHEN json_type(data_json, '$.body') IS NOT NULL
            AND json_type(data_json, '$.body') != 'null'
       THEN json_extract(data_json, '$.body')
       ELSE json_extract(data_json, '$.payload.body')
  END AS body,
  -- decrypted_text: root + payload fallback (matches Rust extraction)
  CASE WHEN json_type(data_json, '$.decrypted_text') IS NOT NULL
            AND json_type(data_json, '$.decrypted_text') != 'null'
       THEN json_extract(data_json, '$.decrypted_text')
       ELSE json_extract(data_json, '$.payload.decrypted_text')
  END AS decrypted_text,
  CASE WHEN json_type(data_json, '$.richTextJSON') IS NOT NULL
            AND json_type(data_json, '$.richTextJSON') != 'null'
       THEN json_extract(data_json, '$.richTextJSON')
       ELSE json_extract(data_json, '$.payload.richTextJSON')
  END AS rich_text_json,
  CASE WHEN json_type(data_json, '$.timeZone') IS NOT NULL
            AND json_type(data_json, '$.timeZone') != 'null'
       THEN json_extract(data_json, '$.timeZone')
       ELSE json_extract(data_json, '$.payload.timeZone')
  END AS time_zone,
  CASE WHEN json_type(data_json, '$.tags') IS NOT NULL
            AND json_type(data_json, '$.tags') != 'null'
       THEN json_extract(data_json, '$.tags')
       ELSE json_extract(data_json, '$.payload.tags')
  END AS tags_json,
  CASE WHEN json_type(data_json, '$.moments') IS NOT NULL
            AND json_type(data_json, '$.moments') != 'null'
       THEN json_extract(data_json, '$.moments')
       ELSE json_extract(data_json, '$.payload.moments')
  END AS moments_json,
  CASE WHEN json_type(data_json, '$.clientMeta') IS NOT NULL
            AND json_type(data_json, '$.clientMeta') != 'null'
       THEN json_extract(data_json, '$.clientMeta')
       ELSE json_extract(data_json, '$.payload.clientMeta')
  END AS client_meta_json,
  -- Merge unknown fields from payload and root (root wins on conflicts),
  -- stripping all known and reserved keys from both sides.
  -- Guard json_remove: only strip keys when payload is actually a JSON object;
  -- non-object payloads (string, number, array) would cause json_remove to error.
  json_patch(
    CASE WHEN json_type(data_json, '$.payload') = 'object'
      THEN json_remove(
        json_extract(data_json, '$.payload'),
        '$.id',
        '$.deleted_at',
        '$.date',
        '$.title',
        '$.body',
        '$.decrypted_text',
        '$.richTextJSON',
        '$.timeZone',
        '$.tags',
        '$.moments',
        '$.clientMeta',
        '$.journal_id',
        '$.updated_at',
        '$.user_edit_date',
        '$.is_deleted',
        '$.needs_sync',
        '$.revision'
      )
      ELSE json('{}')
    END,
    json_remove(
      data_json,
      '$.id',
      '$.deleted_at',
      '$.date',
      '$.title',
      '$.body',
      '$.decrypted_text',
      '$.richTextJSON',
      '$.timeZone',
      '$.tags',
      '$.moments',
      '$.clientMeta',
      '$.journal_id',
      '$.updated_at',
      '$.user_edit_date',
      '$.is_deleted',
      '$.needs_sync',
      '$.revision',
      '$.payload'
    )
  ) AS extra_fields_json,
  data_json
FROM entries;

-- Drop old table and rename new table
DROP TABLE entries;
ALTER TABLE entries_new RENAME TO entries;

-- Create indexes for commonly queried fields
CREATE INDEX IF NOT EXISTS idx_entries_journal_id ON entries(journal_id);
CREATE INDEX IF NOT EXISTS idx_entries_date ON entries(date) WHERE date IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_entries_user_edit_date ON entries(user_edit_date) WHERE user_edit_date IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_entries_deleted ON entries(is_deleted, deleted_at);
CREATE INDEX IF NOT EXISTS idx_entries_journal_date ON entries(journal_id, date DESC) WHERE is_deleted = 0;
CREATE INDEX IF NOT EXISTS idx_entries_journal_edit_date ON entries(journal_id, user_edit_date DESC) WHERE is_deleted = 0;
