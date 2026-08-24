CREATE TABLE IF NOT EXISTS entry_embeddings (
  entry_id TEXT NOT NULL PRIMARY KEY,
  journal_id TEXT NOT NULL,
  source_hash TEXT NOT NULL,
  embedding_json TEXT NOT NULL,
  dimension INTEGER NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_entry_embeddings_journal_id
  ON entry_embeddings(journal_id);

CREATE VIRTUAL TABLE IF NOT EXISTS entry_embeddings_vec USING vec0(
  embedding float[384],
  entry_id TEXT PRIMARY KEY,
  +journal_id TEXT
);

WITH missing_source AS (
  SELECT e.entry_id, e.journal_id, e.embedding_json, e.updated_at, e.rowid
  FROM entry_embeddings e
  WHERE e.dimension = 384
    AND NOT EXISTS (
      SELECT 1
      FROM entry_embeddings_vec v
      WHERE v.entry_id = e.entry_id
    )
),
ranked AS (
  SELECT
    entry_id,
    journal_id,
    embedding_json,
    ROW_NUMBER() OVER (
      PARTITION BY entry_id
      ORDER BY updated_at DESC, rowid DESC
    ) AS rn
  FROM missing_source
),
dedup AS (
  SELECT entry_id, journal_id, embedding_json
  FROM ranked
  WHERE rn = 1
)
INSERT INTO entry_embeddings_vec(entry_id, journal_id, embedding)
SELECT d.entry_id, d.journal_id, d.embedding_json
FROM dedup d;
