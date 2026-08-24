# 004 — Enable WAL mode and reduce per-operation connection overhead

**Source:** architecture.md § The Local Store — critique: one connection per operation, no WAL mode  
**Size:** S  
**Depends on:** nothing (independent improvement)

---

## Problem

The `Store` opens a new SQLite connection for every public method call. This is correct but carries overhead for workflows (sync, list with embeddings) that call dozens of store methods in sequence. Additionally, SQLite defaults to `DELETE` journal mode, which is slower for read-heavy workloads than WAL mode.

Two specific issues:
1. Connection open/close cost is paid on every store call.
2. WAL mode is never set, so concurrent readers block on writes.

## Goal

Enable WAL mode on connection open and evaluate whether a shared `Connection` (or `r2d2`/`rusqlite_pool`) would reduce overhead without introducing complexity.

## Concrete steps

1. **Enable WAL mode immediately.** In `Store::open_connection()` (the shared helper in `sqlite.rs`), add:
   ```rust
   conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
   ```
   This is a one-line change with a measurable throughput improvement for sync workloads. Verify with `cargo test --locked`.

2. **Evaluate connection pooling.** Profile a full sync run to see how much time is spent opening connections vs doing actual SQL. If connection overhead is significant (>5% of sync time):
   - Add `r2d2` and `r2d2_sqlite` to `Cargo.toml`.
   - Replace the `Store` path field with an `r2d2::Pool<SqliteConnectionManager>`.
   - Update `Store::open_default` and `Store::open_at` to initialise a pool with `max_size(4)` or similar.
   - Change all `open_connection()` calls to `self.pool.get()?`.

3. If connection pooling is added, confirm that WAL mode is still set on pool connection creation (set it in the pool's `connection_customizer`).

4. Confirm `cargo test --locked` passes. Confirm sync still produces correct output on a staging environment.

## Notes

- WAL mode is a low-risk, high-value change and should be done regardless of the pooling decision.
- The pooling change is worth doing only if profiling shows it matters. Don't add complexity speculatively.
- WAL mode persists in the database file after first set; subsequent opens do not need to re-set it, but setting it idempotently on every open is harmless.

## Definition of done

- `PRAGMA journal_mode=WAL` is set on every new connection.
- `cargo test --locked` passes.
- (Optional, if profiling justifies it) Connection pool is in place and tests pass.
