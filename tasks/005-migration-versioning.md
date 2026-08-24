# 005 — Add migration versioning and run migrations once per command

**Source:** architecture.md § The Local Store — critique: migrations re-run on every store open, no version tracking  
**Size:** M  
**Depends on:** 004 (WAL mode should be in place before migration changes to avoid locking issues during migration runs)

---

## Problem

All six migration SQL files re-run on every store open because there is no version tracking table. Three consequences:

1. Every `CREATE TABLE` must be written as `IF NOT EXISTS`, which prevents any migration from ever using `ALTER TABLE ... ADD COLUMN` (it would error on re-run) or `INSERT` without `OR IGNORE`.
2. A command like `dayone sync` may open the store many times internally. Migrations re-execute on each open, adding unnecessary overhead.
3. There is no audit trail of which migrations have been applied.

## Goal

Add a `schema_migrations` tracking table. Check which migrations have been applied and skip them. Run the migration check exactly once per command invocation (at `Store::initialize()` time), not per-connection.

## Concrete steps

1. Add a bootstrap SQL block to `Store::initialize()` that runs unconditionally before the numbered migrations:
   ```sql
   CREATE TABLE IF NOT EXISTS schema_migrations (
       version INTEGER PRIMARY KEY,
       applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
   );
   ```

2. Change `Store::initialize()` to check for each migration version before running it:
   ```rust
   fn run_migration_if_needed(conn: &Connection, version: u32, sql: &str) -> Result<(), StoreError> {
       let already_applied: bool = conn.query_row(
           "SELECT 1 FROM schema_migrations WHERE version = ?1",
           params![version],
           |_| Ok(true),
       ).optional()?.is_some();

       if !already_applied {
           conn.execute_batch(sql)?;
           conn.execute(
               "INSERT INTO schema_migrations (version) VALUES (?1)",
               params![version],
           )?;
       }
       Ok(())
   }
   ```

3. Call `run_migration_if_needed` for each numbered migration file (versions 1–6) in order.

4. Remove the `IF NOT EXISTS` guard from SQL statements that no longer need it (do this carefully; keep `IF NOT EXISTS` on the initial bootstrap table itself, but it is now optional elsewhere). Do not remove guards in this task — leave that cleanup for a follow-up to keep the diff reviewable.

5. Ensure `Store::initialize()` is called once at command startup and is **not** called inside individual store methods. Audit the codebase to confirm no store method calls `initialize()` internally. If `open_connection()` currently calls `initialize()`, change it so only `Store::open_default()` and `Store::open_at()` call it.

6. Run `cargo test --locked`. Verify that running the CLI twice against the same database does not re-run migrations.

## Notes

- The existing six migration files can stay as embedded SQL — no need to change them as part of this task. The versioning layer sits above them.
- Future migrations (version 7+) can freely use non-idempotent SQL because they will only run once.

## Definition of done

- `schema_migrations` table is created and populated on first run.
- Subsequent runs skip already-applied migrations.
- `Store::initialize()` is called exactly once per command invocation.
- `cargo test --locked` passes.
