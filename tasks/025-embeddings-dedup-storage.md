# 025 — Eliminate duplicate embedding storage (`embedding_json` column)

**Source:** architecture.md § Embeddings and Semantic Search — critique: embedding JSON stored twice  
**Size:** S  
**Depends on:** 024 (chunked embeddings changes the schema; this cleanup should happen after or alongside that migration)

---

## Problem

Each embedding is stored twice:
1. As a JSON array in `entry_embeddings.embedding_json` (a `TEXT` column)
2. As a binary float vector in the `entry_embeddings_vec` sqlite-vec virtual table

At 384 floats × 4 bytes = ~1.5 KB per vector, and with JSON encoding overhead, the `embedding_json` column adds ~2–3 KB per chunk. For a journal with 10,000 entries, this is ~20–30 MB of redundant storage.

The `entry_embeddings_vec` virtual table is the operational index; it is what search queries run against. The `embedding_json` column appears to be kept as a portability backup, but there is no code path that reads it back for operational use.

## Goal

Remove the `embedding_json` column. The sqlite-vec virtual table is the single embedding store. If there is a portability use case (e.g., exporting embeddings for debugging), add an explicit export function rather than storing redundantly at all times.

## Concrete steps

1. Audit all usages of `embedding_json` in the codebase:
   ```
   grep -r "embedding_json" src/
   ```
   Confirm it is only written (never read in production paths) or identify any read paths.

2. If `embedding_json` is only written (no read paths in production):
   - Remove the column from the `upsert_entry_embedding` insert statement.
   - Add migration version 8 (or combine with 024's migration):
     ```sql
     -- SQLite does not support DROP COLUMN before 3.35.0; check the bundled version.
     -- If supported: ALTER TABLE entry_embeddings DROP COLUMN embedding_json;
     -- Otherwise: recreate the table without the column.
     ```
   - Remove the `embedding_json` field from the `EntryEmbeddingUpsert` struct (or equivalent).

3. If `embedding_json` is used in a debug or export path, gate it behind a `--debug` flag or a separate `embeddings export` subcommand rather than storing it by default.

4. Update `list_entries_with_embeddings` if it selects `embedding_json`.

5. Run `cargo test --locked`. Verify that embedding generation and semantic search still work.

## Notes

- SQLite's `DROP COLUMN` requires version 3.35.0+. The project uses bundled SQLite via `rusqlite`; check the bundled version with `rusqlite::version()` or in `Cargo.lock`. If the bundled version is older, recreate the table without the column using the standard SQLite table-rename pattern:
  ```sql
  CREATE TABLE entry_embeddings_new (...); -- without embedding_json
  INSERT INTO entry_embeddings_new SELECT id, ... FROM entry_embeddings;
  DROP TABLE entry_embeddings;
  ALTER TABLE entry_embeddings_new RENAME TO entry_embeddings;
  ```

## Definition of done

- `embedding_json` column is removed from `entry_embeddings`.
- No code writes or reads `embedding_json`.
- Semantic search returns correct results after the migration.
- `cargo test --locked` passes.
