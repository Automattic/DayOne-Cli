## Telemetry / Error Reporting

**Files:** `src/telemetry.rs`, `src/analytics/**`, `src/store/analytics.rs`, error handling near network/auth/sync/crypto paths.

- Sentry/error messages should be categorical. Entity IDs, emails, tokens, home directories, raw API bodies, decrypted content, key material, and ciphertext blobs must not appear in message strings.
- Use scrubbed structured context for diagnostics when needed. Preserve the `cause` chain when wrapping errors so stacks remain useful.
- User errors may be intentionally skipped or reported at lower severity, but broad suppression must not hide real sync/auth/crypto failures.
- Telemetry disable flags (`DAYONE_TELEMETRY`, `DO_NOT_TRACK`) have tested truthy/falsy behavior; do not reinterpret arbitrary values.

### Product analytics (Automattic Tracks)

- `src/analytics/**` queues events into the per-profile SQLite `analytics_events` table and flushes them via a batched `POST` to the Tracks `/rest/v1.1/tracks/record` endpoint. Analytics must never block or fail a command: all `track`/`flush`/`enqueue` paths are best-effort and swallow errors.
- Event property **values** may carry ids/enums/counts/durations, but never journal content, titles, bodies, emails, or tokens. Property **names** must satisfy `^[a-z_][a-z0-9_]*$` (invalid ones are dropped, not sent).
- Sending is gated by `telemetry::is_enabled()` (the same env flags) **and** the server `track_usage_statistics` setting; the default-enabled fallback must not flip to default-disabled or vice versa without intent.
- Analytics queue helpers and `flush` must not recreate a database that `auth logout --force` removed (guarded on `db_path().exists()`).

### What to watch for

- `format!` error messages that interpolate IDs, emails, paths, bearer tokens, request bodies, or payload JSON.
- New broad `is_user_error` or suppression predicates that swallow adjacent unexpected cases.
- Tests that only prove the noisy case is quiet, not that neighboring actionable errors still report.
- Analytics events that attach high-cardinality or sensitive properties, or that send synchronously / block the command path.
