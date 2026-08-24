# 003 — Replace `NonRetryableOutboxError` with a typed `OutboxProcessError` enum

**Source:** code-structure.md § Error Handling  
**Size:** S  
**Depends on:** 002 (StoreError must exist first so OutboxProcessError can wrap it)

---

## Problem

`sync/engine.rs` defines `NonRetryableOutboxError` inline as a hand-rolled struct implementing `std::error::Error`. It is then matched using `err.downcast_ref::<NonRetryableOutboxError>()` — a pattern that requires the caller to know the concrete error type ahead of time, is invisible to `grep` for enum variants, and breaks silently if the wrong type is passed to `anyhow`.

There are at least three distinct outbox failure modes (non-retryable, defer-due-to-missing-keys, and retryable-with-backoff) that are currently distinguished by runtime checks rather than types.

## Goal

Replace the hand-rolled struct with a `thiserror`-derived `OutboxProcessError` enum. The drain function returns this typed error. Callers match on variants instead of downcasting.

## Concrete steps

1. Create (or add to `src/sync/phases/outbox.rs` from task 011, or add inline in `engine.rs` until task 011 lands) the error type:
   ```rust
   #[derive(Debug, thiserror::Error)]
   pub enum OutboxProcessError {
       #[error("non-retryable: {0}")]
       NonRetryable(String),

       #[error("missing encryption keys — deferred")]
       MissingKeys,

       #[error(transparent)]
       Store(#[from] StoreError),

       #[error(transparent)]
       Http(#[from] reqwest::Error),
   }
   ```

2. Change the per-item processing function inside `drain_sync_outbox` to return `Result<(), OutboxProcessError>` instead of `Result<()>` (anyhow).

3. Remove the `NonRetryableOutboxError` struct and its `impl Display` / `impl std::error::Error`.

4. Replace all `err.downcast_ref::<NonRetryableOutboxError>()` call sites with `match err { OutboxProcessError::NonRetryable(_) => ... }`.

5. Replace the `is_missing_active_content_public_key_error` string-matching helper in `crypto.rs` with a check against `OutboxProcessError::MissingKeys`.

6. Run `cargo test --locked` to confirm no regressions.

## Definition of done

- `NonRetryableOutboxError` struct is deleted.
- `is_missing_active_content_public_key_error` helper is removed or repurposed.
- All outbox error-path matching uses enum variant matching, not downcasting.
- `cargo test --locked` passes.
