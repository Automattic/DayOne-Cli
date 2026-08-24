# 006 — Define typed domain structs; deserialise at the store boundary

**Source:** code-structure.md § Untyped Domain Models; architecture.md § The Date and Entry Model — critique: JSON blob hides all entry fields  
**Size:** L  
**Depends on:** 001 (store split), 002 (StoreError)

---

## Problem

Every domain record — journal, entry, daily chat message, context item, user settings — travels through the codebase as a raw JSON string or `serde_json::Value`. Fields are accessed with string subscript operators (`record["fieldName"].as_str()`). A field name typo is a silent `None` at runtime. There are no types that enumerate what an `Entry` or `Journal` actually contains. Renaming a field requires grep-and-pray refactoring.

This is the most significant Rust-idiom violation in the codebase: Rust's type system is its primary correctness tool, and the domain layer opts out of it entirely.

## Goal

Define concrete structs for the primary domain entities. Deserialise from `data_json` at the store read boundary so that all code above the store works with typed values, not `Value` or `String`. Use `#[serde(flatten)]` to preserve unknown fields for safe round-tripping to the server.

## Concrete steps

1. Create `src/models/mod.rs` and declare sub-modules. Start with the highest-traffic entities:

   **`src/models/entry.rs`**
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "camelCase")]
   pub struct Entry {
       pub uuid: String,                          // entry ID
       pub journal_id: String,
       pub date: f64,                             // epoch ms — user-assigned entry date
       pub user_edit_date: Option<String>,        // ISO 8601 — last edited
       pub creation_date: Option<String>,         // ISO 8601 — actual creation time
       pub body: Option<String>,
       pub rich_text_json: Option<serde_json::Value>,
       pub tags: Option<Vec<String>>,
       pub timezone: Option<String>,
       pub is_pinned: Option<bool>,
       pub is_all_day: Option<bool>,
       pub starred: Option<bool>,

       #[serde(flatten)]
       pub extra: std::collections::HashMap<String, serde_json::Value>,
   }
   ```

   **`src/models/journal.rs`**
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "camelCase")]
   pub struct Journal {
       pub uuid: String,
       pub name: Option<String>,
       pub is_encrypted: Option<bool>,
       pub sort_method: Option<String>,
       pub active_content_key_fingerprint: Option<String>,

       #[serde(flatten)]
       pub extra: std::collections::HashMap<String, serde_json::Value>,
   }
   ```

   Add similar structs for `DailyChatRecord`, `ContextItem`, and `UserSettings` as needed.

2. Update store read methods to deserialise before returning. For example, in `src/store/entries.rs`:
   ```rust
   pub fn get_entry(&self, entry_id: &str) -> Result<Option<Entry>, StoreError> {
       let json_str = self.get_json_row_by_id("entries", entry_id)?;
       json_str.map(|s| serde_json::from_str::<Entry>(&s)).transpose()
           .map_err(StoreError::Json)
   }
   ```

3. Update store write methods to accept typed structs and serialise to `data_json` at the boundary:
   ```rust
   pub fn upsert_entry(&self, journal_id: &str, entry: &Entry) -> Result<(), StoreError> {
       let data_json = serde_json::to_string(entry).map_err(StoreError::Json)?;
       self.upsert_entry_json_row(journal_id, &entry.uuid, &data_json)
   }
   ```
   The internal `upsert_entry_json_row` methods (which take raw strings) can remain for the sync engine's bulk-upsert path where parsing every record would be wasteful.

4. Update callers (commands, sync engine) to use the typed structs instead of `Value` subscripts. Do this incrementally — start with the command modules (`list.rs`, `entry_write.rs`), then move to `sync/engine.rs`.

5. Verify round-tripping: write a test that deserialises a real entry JSON fixture (including unknown fields), re-serialises, and confirms the output is byte-for-byte identical.

## Notes

- The `extra` catch-all is critical. Without it, fields added by other clients (native app, web app) will be silently dropped when the CLI syncs an entry it did not create.
- `#[serde(rename_all = "camelCase")]` must match the actual field names in the Day One API JSON. Audit a real entry record before committing to the field names.
- This task will surface many call sites; work through them by module to keep PRs reviewable.

## Definition of done

- `Entry`, `Journal`, `DailyChatRecord`, and `ContextItem` structs exist in `src/models/`.
- Store read methods return typed structs for these entities.
- A round-trip test confirms no fields are dropped.
- `cargo test --locked` passes.
