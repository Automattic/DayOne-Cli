# 010 — Split `sync/engine.rs` into phase modules

**Source:** code-structure.md § The Gravity Wells — sync/engine.rs; § Functions Too Large to Reason About  
**Size:** L  
**Depends on:** 003 (OutboxProcessError), 008 (storage traits)

---

## Problem

`src/sync/engine.rs` is 2,885 lines. The `run_sync` function is a single sequential async block of ~600 lines containing: lock acquisition, outbox draining, per-resource pull orchestration, decryptor initialisation, per-journal entry cursor loops, conflict retry accumulation, embedding generation, and lock release. Reasoning about any one part requires holding the full function in working memory. There are no unit tests for any part of it because everything is entangled.

## Goal

Break `run_sync` into clearly named phase functions, each in its own file under `src/sync/phases/`. The `run_sync` orchestrator becomes a coordinator of ~100 lines that calls each phase in sequence. Each phase is independently readable and testable.

## Concrete steps

1. Create `src/sync/phases/mod.rs` declaring three sub-modules.

2. **`src/sync/phases/outbox.rs`** — extract `drain_sync_outbox` and all its helpers:
   - The drain loop (batch lease → process item → mark result)
   - Per-item dispatch by `(resource, operation)` tuple
   - Retry/defer/fail classification logic
   - Signature: `pub async fn drain_sync_outbox(store: &impl OutboxStore, api: &impl DayOneApiClient, run_id: i64, profile_id: i64, resources: &mut Vec<ResourceSyncOutput>, label: &str) -> anyhow::Result<()>`

3. **`src/sync/phases/pull.rs`** — extract the resource pull sequence:
   - `run_cursor_resource` generic helper
   - `run_singleton_resource` generic helper
   - The full pull sequence (user_key → content_keys → journals → entries per journal → user_profile → notifications → … → feature-gated resources)
   - Signature: `pub async fn pull_all_resources(store: &Store, api: &impl DayOneApiClient, run_id: i64, session: &AuthSession, resources: &mut Vec<ResourceSyncOutput>, embed_entries: bool) -> anyhow::Result<Vec<PushRetry>>`
   - Returns accumulated `PushRetry` items for the push phase.

4. **`src/sync/phases/push.rs`** — extract the conflict push-retry flush:
   - Takes the `Vec<PushRetry>` returned by the pull phase
   - Pushes each retry to the server and records results
   - Signature: `pub async fn flush_push_retries(retries: Vec<PushRetry>, api: &impl DayOneApiClient, store: &Store, resources: &mut Vec<ResourceSyncOutput>) -> anyhow::Result<()>`

5. Reduce `sync/engine.rs::run_sync` to the coordinator:
   ```rust
   pub async fn run_sync(store: &Store, base_url: &str, ignore_existing_cursors: bool, embed_entries: bool) -> Result<SyncOutput> {
       // acquire lock, reset stale leases
       phases::outbox::drain_sync_outbox(store, &api, run_id, session.profile_id, &mut resources, "outbox:pre").await?;
       let retries = phases::pull::pull_all_resources(store, &api, run_id, &session, &mut resources, embed_entries).await?;
       phases::push::flush_push_retries(retries, &api, store, &mut resources).await?;
       phases::outbox::drain_sync_outbox(store, &api, run_id, session.profile_id, &mut resources, "outbox:post").await?;
       // release lock, return output
   }
   ```

6. Write unit tests for the outbox phase using `FakeOutboxStore` and `FakeApiClient` (from tasks 008 and 009):
   - Test: item succeeds on first attempt.
   - Test: item retries after 409 with exponential backoff.
   - Test: item is dead-lettered after `OUTBOX_MAX_ATTEMPTS`.
   - Test: item is deferred when `OutboxProcessError::MissingKeys` is returned.

7. Run `cargo test --locked`.

## Notes

- This task does **not** change the sync ordering (pre-drain vs post-drain). That is a separate concern covered in task 011.
- Move `JournalDecryptor`, `EntriesDecryptor`, `PushRetry`, and `NonRetryableOutboxError` types to their respective phase files.
- `log_sync` helper can stay in `engine.rs` and be used from all phases via `use crate::sync::engine::log_sync` or moved to a shared `sync/utils.rs`.

## Definition of done

- `src/sync/phases/outbox.rs`, `pull.rs`, `push.rs` exist and contain the extracted logic.
- `sync/engine.rs::run_sync` is under 120 lines.
- Unit tests for outbox retry logic pass without a database or network.
- `cargo test --locked` passes.
