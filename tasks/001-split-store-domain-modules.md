# 001 — Split `store/sqlite.rs` into domain modules

**Source:** code-structure.md § The Gravity Wells  
**Size:** L  
**Depends on:** nothing — this is the foundation task

---

## Problem

`src/store/sqlite.rs` is 3,414 lines with 64 public methods on a single `impl Store` block. It covers at least seven distinct responsibilities: profiles, auth sessions, sync cursors/locks/run-logging, sync resource state, entry persistence, entry embeddings, entry attachments, outbox lifecycle, and encryption key storage. Every new feature touching persistence gets pulled into this one file because there is nowhere else to put it.

## Goal

Break the single file into domain-focused modules under `src/store/`, keeping `Store` as the one type callers hold. Rust allows a type to have `impl` blocks in multiple files within the same module, so the public API surface of `Store` does not change — callers import `Store` and call the same methods as before.

## Concrete steps

1. Create the target file structure:
   ```
   src/store/
   ├── mod.rs          — re-exports Store; declares the sub-modules
   ├── sqlite.rs       — Store struct definition, open_default/open_at, initialize(), shared helpers (open_connection, run_migrations)
   ├── profiles.rs     — Profile, AuthSession structs; impl Store { list_profiles, get_active_profile, set_active_profile, get_or_create_profile_for_base_url, save_auth_session, get_auth_session_for_base_url, erase_local_data }
   ├── sync_state.rs   — JournalSyncStatus; impl Store { get_sync_cursor, set_sync_cursor, clear_sync_cursors, try_acquire_sync_lock, release_sync_lock, set_journal_sync_status, enable_journal_sync, get_sync_enabled_journal_ids, set_sync_resource_state_success, set_sync_resource_state_error, create_sync_run, finish_sync_run, save_sync_resource_run, reset_processing_outbox_items, reset_processing_outbox_items_by_ids }
   ├── entries.rs      — EntryListSort, EntryListSortMethod, ListPageRequest, ListPageResult; impl Store { upsert_entry_json_row, upsert_entry_json_row_with_edit_date, get_entry_user_edit_date, list_entries_json_rows_by_journal_id, list_entries_json_rows_by_journal_id_paged, list_journals_json_rows_paged, upsert_entry_embedding, delete_entry_embedding, delete_stale_or_deleted_entry_embeddings, get_entry_embedding_source_hash, list_entries_with_embeddings, search_entry_semantic_scores, upsert_entry_attachment, list_entry_attachments, get_entry_attachment, mark_entry_attachments_associated, mark_entry_attachment_original_uploaded, mark_entry_attachment_error }
   ├── outbox.rs       — OutboxItem, EntryAttachmentUpsert, EntryAttachmentRow structs; impl Store { enqueue_outbox_item, lease_outbox_items, mark_outbox_item_succeeded, mark_outbox_item_retry, mark_outbox_item_deferred, mark_outbox_item_failed }
   ├── json_rows.rs    — ListJsonTable; impl Store { upsert_json_row, upsert_json_row_with_sync_state, upsert_singleton_json_row, upsert_user_settings, get_json_row_by_id, get_daily_chat_row_by_date, get_json_row_needs_sync, delete_json_row_by_id, delete_journal_sync_info, update_entries_journal_id, remap_entry_outbox_journal_id, list_json_rows, list_json_rows_paged }
   └── encryption.rs   — impl Store { set_encryption_key_for_profile, get_encryption_key_for_profile }
   ```

2. Move each method group verbatim — no logic changes, only file movement. Each sub-module file must `use super::Store` (or `use crate::store::sqlite::Store`) to add methods to the type.

3. Update `src/store/mod.rs` to `pub use sqlite::Store` and declare all sub-modules with `mod profiles; mod sync_state;` etc. All types that were previously re-exported from `store/sqlite.rs` must be re-exported from `store/mod.rs` so callers do not need to change their imports.

4. Run `cargo build` — it should compile clean. Run `cargo test` — all tests should pass. No behaviour changes.

5. As part of this split, audit visibility of all 64 methods. Any method that is only called from within the `src/store/` module should be downgraded from `pub` to `pub(super)` or private. Document the visibility decisions in a brief comment at the top of each sub-module file.

## Definition of done

- `src/store/sqlite.rs` is under 300 lines (struct definition, constructor, migration runner, shared connection helper only).
- Each domain file is under 600 lines.
- `cargo test --locked` passes unchanged.
- No callers outside `src/store/` needed to change their import paths.
