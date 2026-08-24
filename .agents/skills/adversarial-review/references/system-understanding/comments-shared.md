## Comments / Shared Journals

**Files:** `src/commands/comment*.rs`, `src/comment_codec.rs`, shared journal creation, comment sync code.

### Comments

- Comment commands should verify journal/entry/comment relationships before staging writes, updates, deletes, or reactions.
- Local IDs may be reconciled to server IDs; remaps must update table columns, embedded JSON, reactions, and queued outbox payloads where applicable.
- Comment pagination needs stable created-at/update cursor semantics and should reject legacy or malformed cursor payloads rather than skipping unpredictably.
- Deleted comments and reactions can remain in local storage for sync/history but should be projected correctly for list output.

### Shared journals

- Participant public keys come from the crypto key row matching the grant/user fingerprint, not necessarily the user's latest/current key.
- `all_members_can_edit` defaults to false. Omitting it on update does not reset prior true, only owners may send it, and server feature flags can mask it to false.
- Shared journal creation can be plaintext or E2E depending on explicit flags and profile key state; conflicting flags must remain hard errors.

### What to watch for

- Command paths that trust user-supplied journal/entry IDs without checking local rows.
- Reconciliation that updates visible comment IDs but leaves outbox payloads stale.
- Shared journal key selection by "latest" rather than matching fingerprint.
- Tests that verify staging but not queued payload shape.
