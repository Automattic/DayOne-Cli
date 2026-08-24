# 002 — Introduce typed `StoreError` at the store boundary

**Source:** code-structure.md § Error Handling  
**Size:** M  
**Depends on:** 001 (store split into domain modules)

---

## Problem

Every `Store` method returns `anyhow::Result<T>`. The actual `rusqlite::Error` or `serde_json::Error` is wrapped and its type is lost. Callers cannot distinguish "row not found" from "constraint violation" from "database locked" — they receive an opaque `anyhow::Error` with a string message. This makes error-specific recovery impossible without fragile string matching.

## Goal

Replace `anyhow::Result<T>` at all `Store` method boundaries with `Result<T, StoreError>`, where `StoreError` is a typed enum using `thiserror`. The CLI's top-level `run()` function continues to use `anyhow` — the conversion happens automatically via `From`.

## Concrete steps

1. Add `thiserror` to `Cargo.toml` if not already present.

2. Create `src/store/error.rs`:
   ```rust
   #[derive(Debug, thiserror::Error)]
   pub enum StoreError {
       #[error("not found: {table}/{id}")]
       NotFound { table: String, id: String },

       #[error("database error: {0}")]
       Sqlite(#[from] rusqlite::Error),

       #[error("serialisation error: {0}")]
       Json(#[from] serde_json::Error),

       #[error("migration error: {0}")]
       Migration(String),
   }
   ```

3. Re-export `StoreError` from `src/store/mod.rs`.

4. Change each `Store` method signature from `-> Result<T>` (anyhow) to `-> Result<T, StoreError>`. Work through one domain module at a time (profiles first, then sync_state, etc.) to keep diffs reviewable.

5. Replace `anyhow::bail!` and `.context(...)` calls inside store methods with `StoreError` variants or `return Err(StoreError::...)`.

6. For callers outside `src/store/` (commands, sync engine): the `?` operator continues to work because `anyhow::Error` implements `From<StoreError>` automatically. No caller changes are required except where a caller now wants to match on specific variants.

7. Update the existing store tests in `sqlite.rs` to expect `StoreError` variants where specific errors are asserted.

## Definition of done

- All `Store` public methods return `Result<T, StoreError>`.
- `anyhow` is no longer imported in any `src/store/` file except as a target type for conversion at the CLI boundary.
- `cargo test --locked` passes.
- The sync engine's `downcast_ref::<NonRetryableOutboxError>()` pattern is not yet replaced here (that is task 004), but the groundwork is in place.
