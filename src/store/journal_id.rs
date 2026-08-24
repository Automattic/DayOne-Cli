//! Journal-id reconciliation.
//!
//! The CLI uses a single mutable journal id: a locally-created journal starts
//! with a `pending-…` id and adopts the server id on first sync. Because that id
//! is the primary/foreign key everywhere, adopting the server id means rewriting
//! every row that referenced the old id.
//!
//! Rather than maintaining a hand-curated list of those places (and silently
//! orphaning data whenever a new one is added), [`Store::remap_journal_id`]
//! discovers them from the schema at runtime:
//!
//! 1. **Tables** — every non-virtual table with a `journal_id` column is updated
//!    automatically. New tables are covered without code changes.
//! 2. **Embeddings vec** — `entry_embeddings_vec` is a vec0 *virtual* table
//!    (skipped by the table sweep); its rows are rebuilt from the now-remapped
//!    `entry_embeddings`.
//! 3. **Outbox** — queued items carry the journal id in their `object_id` and
//!    payload, not a column, so they are rewritten separately.

use rusqlite::params;

use crate::entry_embeddings::ENTRY_EMBEDDING_DIMENSION;
use crate::store::StoreResult;
use crate::store::sqlite::Store;

/// Tables with a `journal_id` column that are reconciled elsewhere and must be
/// left out of the automatic sweep. `journal_sync_info` is per-journal sync
/// bookkeeping that the create-push flow deletes and re-creates explicitly.
const EXCLUDED_TABLES: &[&str] = &["journal_sync_info"];

/// What [`Store::remap_journal_id`] changed, for logging/telemetry.
#[derive(Debug, Default)]
pub struct JournalIdRemap {
    /// `(table, rows_updated)` for each table that had matching rows.
    pub tables: Vec<(String, usize)>,
    /// Rows reinserted into `entry_embeddings_vec`.
    pub embeddings_rebuilt: usize,
    /// Outbox items whose id/payload referenced the journal.
    pub outbox_items: usize,
}

impl Store {
    /// Rewrites every reference to `from_journal_id` so it points at
    /// `to_journal_id`, across all `journal_id` columns, the embeddings vec
    /// table, and the sync outbox. Idempotent and a no-op when the ids match.
    pub fn remap_journal_id(
        &self,
        from_journal_id: &str,
        to_journal_id: &str,
    ) -> StoreResult<JournalIdRemap> {
        let mut summary = JournalIdRemap::default();
        if from_journal_id == to_journal_id {
            return Ok(summary);
        }

        let mut conn = self.connect()?;
        let tx = conn.transaction()?;

        // 1. Every non-virtual table that stores a journal id as a column.
        for table in journal_id_tables(&tx)? {
            if EXCLUDED_TABLES.contains(&table.as_str()) {
                continue;
            }
            // Table names come from sqlite_master (not user input) and are quoted.
            let sql = format!(r#"UPDATE "{table}" SET journal_id = ?2 WHERE journal_id = ?1"#);
            let rows = tx.execute(&sql, params![from_journal_id, to_journal_id])?;
            if rows > 0 {
                summary.tables.push((table, rows));
            }
        }

        // 2. entry_embeddings_vec is virtual (skipped above). Drop the stale rows
        //    and rebuild from the now-remapped entry_embeddings table. Clear both
        //    ids (not just `from`) so a re-run after a crash can't hit the
        //    entry_id PK on reinsert — this keeps the whole remap idempotent.
        tx.execute(
            "DELETE FROM entry_embeddings_vec WHERE journal_id = ?1 OR journal_id = ?2",
            params![from_journal_id, to_journal_id],
        )?;
        summary.embeddings_rebuilt = tx.execute(
            r#"
            INSERT INTO entry_embeddings_vec (entry_id, journal_id, embedding)
            SELECT entry_id, journal_id, embedding_json
            FROM entry_embeddings
            WHERE journal_id = ?1 AND dimension = ?2
            "#,
            params![to_journal_id, ENTRY_EMBEDDING_DIMENSION as i64],
        )?;

        // 3. Outbox items reference the journal via object_id + payload, not a column.
        summary.outbox_items = remap_outbox_rows(&tx, from_journal_id, to_journal_id)?;

        tx.commit()?;
        Ok(summary)
    }
}

/// Non-virtual tables that have a `journal_id` column, discovered from the schema.
fn journal_id_tables(tx: &rusqlite::Transaction) -> StoreResult<Vec<String>> {
    let mut stmt = tx.prepare(
        r#"
        SELECT m.name
        FROM sqlite_master m
        WHERE m.type = 'table'
          AND m.sql NOT LIKE 'CREATE VIRTUAL TABLE%'
          AND EXISTS (
            SELECT 1 FROM pragma_table_info(m.name) WHERE name = 'journal_id'
          )
        ORDER BY m.name
        "#,
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut tables = Vec::new();
    for row in rows {
        tables.push(row?);
    }
    Ok(tables)
}

/// Rewrites the journal id wherever it appears in queued outbox items: the
/// leading segment of `object_id` (`{journal}` or `{journal}:{...}`) and the
/// `journal_id`/`id` fields of the payload.
fn remap_outbox_rows(tx: &rusqlite::Transaction, from: &str, to: &str) -> StoreResult<usize> {
    let mut pending = Vec::new();
    {
        let mut stmt =
            tx.prepare("SELECT id, resource, operation, object_id, payload_json FROM sync_outbox")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            pending.push(row?);
        }
    }

    let object_prefix = format!("{from}:");
    let mut changed = 0;
    for (id, resource, operation, object_id, payload_json) in pending {
        // The journal's own create item is the one being completed right now: its
        // payload is compared against the lease-time snapshot by
        // `mark_outbox_item_succeeded`. Rewriting it would break that match and
        // revert the item to pending, re-POSTing the journal forever. Leave it
        // untouched — it is deleted as soon as the create push succeeds. Dependent
        // items (entries, media) are reset and re-leased after the create, so
        // remapping them here is safe.
        if resource == "journal" && operation == "create" {
            continue;
        }

        let mut touched = false;

        let new_object_id = if object_id == from || object_id.starts_with(&object_prefix) {
            touched = true;
            object_id.replacen(from, to, 1)
        } else {
            object_id
        };

        let new_payload = match serde_json::from_str::<serde_json::Value>(&payload_json) {
            Ok(mut value) => {
                if let Some(obj) = value.as_object_mut() {
                    for key in ["journal_id", "id"] {
                        if obj.get(key).and_then(serde_json::Value::as_str) == Some(from) {
                            obj.insert(key.to_owned(), serde_json::Value::String(to.to_owned()));
                            touched = true;
                        }
                    }
                }
                value.to_string()
            }
            Err(_) => payload_json,
        };

        if touched {
            tx.execute(
                "UPDATE sync_outbox SET object_id = ?2, payload_json = ?3, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![id, new_object_id, new_payload],
            )?;
            changed += 1;
        }
    }
    Ok(changed)
}
