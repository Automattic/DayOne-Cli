# 024 — Implement chunk-based embeddings for long entries

**Source:** architecture.md § Embeddings and Semantic Search — critique: whole-entry embeddings become watered-down for long entries  
**Size:** M  
**Depends on:** 006 (typed domain models, so entries are easier to split by field), 001 (store split, so embedding storage methods are in entries.rs)

---

## Problem

Each entry is embedded as a single vector by concatenating all available text fields (`title`, `body`, `payload.body`) into one string and running inference once. For long entries, the resulting embedding is an average of all the content. Semantic search queries for a specific topic or passage will match poorly against entries where that topic is one of many, because the specific signal is diluted.

## Goal

Chunk long entries into overlapping windows of N words. Each chunk gets its own embedding stored with the word range it covers. Short entries produce one chunk. Search results identify both the entry and the matching chunk, enabling sub-entry result highlighting in the future.

## Concrete steps

1. Define chunking constants (make these configurable via a const or compile-time feature):
   ```rust
   const CHUNK_SIZE_WORDS: usize = 200;    // words per chunk
   const CHUNK_OVERLAP_WORDS: usize = 40;  // overlap between adjacent chunks
   ```

2. Implement a `chunk_text` function in `src/entry_embeddings.rs`:
   ```rust
   pub struct TextChunk {
       pub text: String,
       pub word_start: usize,
       pub word_end: usize,
   }

   pub fn chunk_text(text: &str) -> Vec<TextChunk>;
   ```
   Split on whitespace, create windows of `CHUNK_SIZE_WORDS` words with `CHUNK_OVERLAP_WORDS` overlap. Return a single chunk for texts under `CHUNK_SIZE_WORDS` words.

3. Update the `entry_embeddings` schema to support multiple chunks per entry. Add a migration (version 7) for:
   ```sql
   ALTER TABLE entry_embeddings ADD COLUMN chunk_index INTEGER NOT NULL DEFAULT 0;
   ALTER TABLE entry_embeddings ADD COLUMN word_start INTEGER;
   ALTER TABLE entry_embeddings ADD COLUMN word_end INTEGER;
   ```
   Change the primary key from `entry_id` to `(entry_id, chunk_index)`.

4. Update the `entry_embeddings_vec` virtual table accordingly — each row is now `(entry_id, chunk_index)` identifying a specific chunk.

5. Update `compute_entry_embedding` to call `chunk_text`, run inference on each chunk, and return `Vec<(chunk_index, TextChunk, Vec<f32>)>`.

6. Update `store/entries.rs` (`upsert_entry_embedding`, `delete_entry_embedding`, `list_entries_with_embeddings`) to work with chunk-indexed rows.

7. Update `search_entry_semantic_scores` to run KNN over all chunks, deduplicate by `entry_id` (taking the best chunk score per entry), and return entry-level results.

8. Update `delete_stale_or_deleted_entry_embeddings` and `get_entry_embedding_source_hash` to handle chunked rows.

9. Add a migration step in the chunk-index migration that deletes all existing single-vector embeddings (they will be regenerated on the next `sync --embed-entries` or `embeddings recalculate-entries`).

10. Write unit tests for `chunk_text`:
    - Empty string → one empty chunk.
    - 100 words → one chunk.
    - 400 words → three chunks with correct overlap.

11. Run `cargo test --locked`.

## Notes

- The `query:` prefix for search queries (already used in the MiniLM asymmetric model) must still be applied to the query embedding, not to document chunks.
- Sub-entry result highlighting (returning the chunk text in search results) is a future enhancement; this task only stores chunk ranges.

## Definition of done

- `chunk_text` splits entries into overlapping windows.
- Each chunk is stored as a separate row in `entry_embeddings` and `entry_embeddings_vec`.
- Search results deduplicate by entry, returning the best chunk score.
- `chunk_text` has unit tests.
- `cargo test --locked` passes.
