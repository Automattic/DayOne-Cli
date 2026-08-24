# 011 — Fix sync ordering: drain outbox only after a full pull

**Source:** architecture.md § The Sync Engine — critique: outbox is drained before the pull completes  
**Size:** S  
**Depends on:** nothing (ordering fix is self-contained; can be done before or after 010)

---

## Problem

The current sync sequence is:

```
1. acquire lock
2. drain outbox  ← pushes local writes to server BEFORE fetching latest server state
3. pull all resources
4. push conflict retries
5. drain outbox  ← again, after pull
6. release lock
```

Draining the outbox before the pull means local writes are pushed to the server before the latest server state has been fetched. If the server has a newer version of an entry that the local outbox is about to overwrite, this creates a conflict that would not have existed had the pull run first. The pre-pull drain is the primary source of avoidable sync conflicts.

## Goal

Remove the pre-pull outbox drain. The outbox is drained once, at the end of sync, after a complete pull.

**Proposed sequence:**
```
1. acquire lock
2. pull all resources   ← complete pull first
3. drain outbox         ← push local writes against fully up-to-date local state
4. release lock
```

## Concrete steps

1. In `src/sync/engine.rs` (or `src/sync/phases/outbox.rs` after task 010), locate the `drain_sync_outbox` call labeled `"outbox:pre"`.

2. Remove the pre-pull drain call entirely.

3. Keep the post-pull drain call (labeled `"outbox:post"`).

4. Remove the now-unused `"outbox:pre"` label string constant if it exists.

5. Verify the sync output JSON no longer includes an `"outbox:pre"` resource entry.

6. Test manually against staging:
   - Write an entry locally (`dayone entry write ...`).
   - Run `dayone sync`.
   - Confirm the entry is uploaded and the server version matches.
   - Run `dayone sync` again from a second profile targeting the same account.
   - Confirm the entry appears in the second profile after sync.

7. Run `cargo test --locked`.

## Notes

- The post-pull drain also handles conflict retries today (via `push_retries`). After task 010, conflict retries are handled by the push phase before the final outbox drain. The ordering then becomes: pull → push retries → drain outbox.
- This is a low-risk change: removing a pre-pull drain can only reduce conflicts, not create new ones.

## Definition of done

- The pre-pull `drain_sync_outbox` call is removed.
- `run_sync` output no longer includes an `"outbox:pre"` resource in normal operation.
- `cargo test --locked` passes.
- Manual staging test confirms entries are synced correctly.
