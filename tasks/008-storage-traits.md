# 008 — Introduce storage traits (`OutboxStore`, `SyncStateStore`, `EntryStore`)

**Source:** code-structure.md § No Trait Boundaries  
**Size:** M  
**Depends on:** 006 (typed domain models), 002 (StoreError)

---

## Problem

The entire codebase passes `&Store` (a concrete type) everywhere. There is no way to test sync logic, outbox drain behaviour, or entry write logic without a real SQLite database. The `Store` cannot be swapped for an in-memory double in tests.

## Goal

Define traits for the domain slices that the sync engine and command modules actually need. Implement them on `Store`. Pass trait objects (or generic type parameters) to the sync engine and to any command that needs testability. Write the first in-memory fake implementations to enable unit tests.

## Concrete steps

1. Create `src/store/traits.rs` (or co-locate each trait in its domain module). Define at minimum:

   **`OutboxStore`** — used by `sync/engine.rs` and `commands/entry_write.rs`:
   ```rust
   pub trait OutboxStore {
       fn enqueue_outbox_item(&self, resource: &str, operation: &str, object_id: &str, payload_json: &str) -> Result<(), StoreError>;
       fn lease_outbox_items(&self, now_epoch_ms: i64, limit: usize) -> Result<Vec<OutboxItem>, StoreError>;
       fn mark_outbox_item_succeeded(&self, id: &str, leased_payload_json: &str) -> Result<(), StoreError>;
       fn mark_outbox_item_retry(&self, id: &str, next_attempt_epoch_ms: i64) -> Result<(), StoreError>;
       fn mark_outbox_item_deferred(&self, id: &str, retry_after_epoch_ms: i64) -> Result<(), StoreError>;
       fn mark_outbox_item_failed(&self, id: &str, error: &str) -> Result<(), StoreError>;
       fn reset_processing_outbox_items(&self, stale_before_epoch_ms: i64) -> Result<(), StoreError>;
   }
   ```

   **`SyncStateStore`** — used by `sync/engine.rs`:
   ```rust
   pub trait SyncStateStore {
       fn get_sync_cursor(&self, resource_key: &str) -> Result<Option<String>, StoreError>;
       fn set_sync_cursor(&self, resource_key: &str, cursor: Option<&str>) -> Result<(), StoreError>;
       fn clear_sync_cursors(&self) -> Result<(), StoreError>;
       fn try_acquire_sync_lock(&self, ttl_ms: i64) -> Result<bool, StoreError>;
       fn release_sync_lock(&self) -> Result<(), StoreError>;
       fn create_sync_run(&self) -> Result<i64, StoreError>;
       fn finish_sync_run(&self, run_id: i64, ok: bool, fatal_error: Option<&str>) -> Result<(), StoreError>;
   }
   ```

   **`EntryReadStore`** — used by `commands/list.rs`, `commands/search.rs`:
   ```rust
   pub trait EntryReadStore {
       fn list_entries_json_rows_by_journal_id(&self, journal_id: &JournalId, sort: EntryListSort, sort_method: EntryListSortMethod, page: ListPageRequest) -> Result<ListPageResult, StoreError>;
       fn get_entry(&self, entry_id: &EntryId) -> Result<Option<Entry>, StoreError>;
   }
   ```

2. Implement each trait on `Store` (this is just re-tagging existing methods as trait implementations; no logic changes).

3. Update `sync/engine.rs` to accept `impl OutboxStore + SyncStateStore` (or `&dyn OutboxStore`, depending on whether the function is generic or dynamic dispatch is preferred). Use `dyn Trait` for functions that already use `&Store` via dynamic call; use generics where performance matters.

4. Write `src/store/fake.rs` (test-only): in-memory implementations of each trait backed by `RefCell<Vec<...>>` or similar. These are used exclusively in unit tests.
   ```rust
   #[cfg(test)]
   pub struct FakeOutboxStore {
       items: RefCell<VecDeque<OutboxItem>>,
       succeeded: RefCell<Vec<String>>,
   }

   #[cfg(test)]
   impl OutboxStore for FakeOutboxStore { ... }
   ```

5. Write at least one unit test using `FakeOutboxStore` to verify outbox retry logic without a real database.

## Notes

- Traits with async methods require either `async-trait` crate or the async trait stabilisation in Rust 1.75+. Check the project's MSRV and choose accordingly.
- Start with `OutboxStore` since it has the clearest test value (retry backoff, non-retryable paths). Add other traits incrementally.

## Definition of done

- `OutboxStore`, `SyncStateStore`, and `EntryReadStore` traits are defined.
- `Store` implements all three.
- `sync/engine.rs` accepts trait objects instead of `&Store` for the outbox path.
- At least one unit test uses `FakeOutboxStore` and passes without a database.
- `cargo test --locked` passes.
