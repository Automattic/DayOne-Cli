## Sync / Outbox

**Files:** `src/sync/**`, `src/commands/sync*.rs`, command code that stages local mutations.

### Local-first contract

- Writes generally update local SQLite and enqueue outbox work. Sync later pushes queued mutations and pulls remote changes.
- Local read commands (`list`, `search`, TUI) depend on prior sync and SQLite state; they should not silently make ad hoc network reads unless the command explicitly says so.

### Outbox invariants

- Enqueue deduplication, retry/defer classification, lease handling, and local payload snapshots protect against duplicate pushes and lost edits.
- Do not mutate processing lease payloads accidentally. Reconciliation updates must be explicit and tested.
- Decode errors for malformed queued payloads are generally non-retryable. Transient network/server errors should preserve retry/backoff state.
- Pushing an entry should prefer the queued payload snapshot when the local row has since been clobbered.

### Cursor and persistence ordering

- Do not advance sync cursors before local persistence succeeds. If the CLI records a cursor for a page it failed to store, later syncs can permanently skip remote data.
- A page with no cursor or a stalled pagination response needs explicit handling; do not infer success from a partially parsed response.
- Deleted/tombstoned rows still matter. Pull phases must persist tombstones and filter them only at read projections.

### Conflict resolution

- Entry conflicts use user edit dates where available, falling back to updated-at semantics. Be explicit about milliseconds vs seconds and string vs numeric payload shapes.
- Daily chat messages merge by message ID using `user_edit_date`; equal timestamps prefer incoming.
- Settings/profile-ish server state often treats server as source of truth after conflict responses.

### Media / promise fulfillment

- Entry sync uploads thumbnail metadata with the entry, while original media is uploaded separately to object storage and later fulfilled server-side.
- Moment metadata (md5, content type, dimensions, type) must let the server match the uploaded file to the entry. Metadata regressions can make media appear locally but remain unavailable on other devices.

### What to watch for

- Cursor advancement before durable store writes.
- Broad `continue`/`ok` paths that hide failed local writes.
- Retry logic that increments/decrements attempts incorrectly or drops backoff/error fields.
- Entry/comment ID remaps that update visible rows but not embedded JSON or queued payloads.
- New sync code that bypasses fake-client/store test patterns.
