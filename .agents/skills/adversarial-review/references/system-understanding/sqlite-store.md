## SQLite Store

**Files:** `src/store/**`, especially `src/store/sqlite.rs` and `src/store/migrations/**`.

### Storage contract

- SQLite is the local source for offline reads. It stores profiles, sessions, journals, entries, comments/reactions, outbox rows, sync cursors, embeddings, and attachment metadata.
- Unknown JSON fields from Day One payloads must round-trip unless intentionally reserved/stripped. Future server fields should not disappear after a local write.
- Deleted/tombstoned content is often stored and filtered by list/search projections rather than physically removed.

### Migrations

- Migrations must be idempotent and safe on existing databases.
- New columns need sensible defaults/backfills that preserve old rows.
- Schema changes are critical because they apply to user-local stores, not just disposable test DBs.

### Profile/session behavior

- Profiles select database location and API host. Active profile state participates in runtime precedence after CLI args and env vars.
- Auth session rows should stay scoped by base URL/profile. Cross-environment token leakage is a security bug.

### Ordering and pagination

- Entry listing can order by entry date or user edit date; cursor pagination must encode enough data to resume without duplicating or skipping rows.
- Payload dates can be numeric, string, RFC3339, or malformed. Tests cover fallback behavior; preserve these edge cases.

### What to watch for

- Changes that drop `extra_fields_json` or rebuild payloads from known fields only.
- Cursor pagination that is not stable under equal sort keys.
- Comment/reaction remaps that update table columns but leave embedded JSON stale.
- SQL built with string concatenation from user-controlled values.
- Tests that use the real config/profile DB instead of temp stores.
