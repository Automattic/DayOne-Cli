# 014 — Route conflict push retries through the outbox

**Source:** architecture.md § The Sync Engine — critique: push retries should route through the outbox  
**Size:** M  
**Depends on:** 003 (OutboxProcessError), 010 (sync phase split)

---

## Problem

During a resource pull, if the server sends a record older than the local version, the engine accumulates a `PushRetry` in a `Vec<PushRetry>`. After all pulls complete, this Vec is flushed in a separate step.

Two issues:
1. **Durability:** If sync crashes between the pull phase and the push-retry flush, the `Vec<PushRetry>` is lost. Those conflicts will not be retried on the next sync run.
2. **Duplication:** The push-retry flush is a second, parallel write path with its own retry/error handling, alongside the outbox which already provides exactly this functionality (durability, retries, backoff, dead-lettering).

## Goal

When a conflict is detected during a pull, re-queue the local item back into the `sync_outbox` as a push operation instead of accumulating it in an in-memory Vec. The outbox already handles retries, backoff, and durability.

## Concrete steps

1. Define an outbox item shape for conflict push retries. These can use the existing `entry`/`update` resource+operation format, or a new dedicated format — whichever is more natural for the outbox processor to handle. Add deduplication: if an `entry`/`update` outbox item for the same `entry_id` already exists and is pending, do not enqueue a duplicate.

2. Update `src/store/outbox.rs` (or the `OutboxStore` trait) to add a deduplication check in `enqueue_outbox_item`:
   ```rust
   pub fn enqueue_outbox_item_if_not_pending(&self, resource: &str, operation: &str, object_id: &str, payload_json: &str) -> Result<bool, StoreError>
   ```
   Returns `true` if enqueued, `false` if a pending item for the same `(resource, operation, object_id)` already exists.

3. In the pull phase (specifically in the conflict detection path in `sync/conflicts.rs` or wherever `local_is_newer` is called), replace the `push_retries.push(...)` call with:
   ```rust
   store.enqueue_outbox_item_if_not_pending("entry", "update", &local_id, &local_payload_json)?;
   ```

4. Remove the `Vec<PushRetry>` accumulator from `run_sync` and `pull_all_resources`.

5. Remove the push-retry flush phase (`phases/push.rs` or its equivalent). The post-pull outbox drain (which already runs) handles these re-queued items.

6. Verify that the outbox processor correctly handles the re-queued retry items and sends them to the right endpoint. The `entry`/`update` outbox processor path should already do this.

7. Test against staging with a scenario that produces a conflict:
   - Write an entry locally.
   - Update the same entry on another client (e.g., web app) to create a server-side version newer than local.
   - Run `dayone sync` and verify the local (newer) version wins and is pushed to the server.

8. Run `cargo test --locked`.

## Notes

- The deduplication check is important: without it, every sync run that detects a conflict adds another outbox item for the same entry, and they pile up.
- This task removes the `push.rs` phase module (from task 010) or makes it empty. That is expected.

## Definition of done

- `Vec<PushRetry>` accumulator is gone.
- Conflict detections are re-queued into the outbox with deduplication.
- The push-retry flush phase is removed.
- Manual conflict scenario on staging works correctly.
- `cargo test --locked` passes.
