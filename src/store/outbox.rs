use rusqlite::{TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::store::StoreResult;
use crate::store::sqlite::Store;

#[derive(Debug, Clone)]
pub struct OutboxItem {
    pub id: String,
    pub resource: String,
    pub operation: String,
    pub object_id: String,
    pub payload_json: String,
    pub attempt_count: i64,
}

/// Full outbox row for inspection (`dayone outbox list`), including retry
/// bookkeeping that the sync drain does not need.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OutboxItemDetails {
    pub id: String,
    pub resource: String,
    pub operation: String,
    pub object_id: String,
    pub status: String,
    pub attempt_count: i64,
    pub next_attempt_epoch_ms: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Raw payload; only fetched on request (payloads can be full entry
    /// snapshots), surfaced as parsed JSON by the CLI, not serialized here.
    #[serde(skip_serializing)]
    pub payload_json: Option<String>,
}

fn enqueue_outbox_item_if_not_pending_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    resource: &str,
    operation: &str,
    object_id: &str,
    payload_json: &str,
) -> StoreResult<bool> {
    let id = format!("{resource}:{operation}:{object_id}");
    let blocked: bool = transaction.query_row(
        r#"
        SELECT EXISTS(
          SELECT 1
          FROM sync_outbox
          WHERE resource = ?1
            AND operation = ?2
            AND object_id = ?3
            AND status IN ('pending', 'processing')
        )
        "#,
        params![resource, operation, object_id],
        |row| row.get(0),
    )?;
    if blocked {
        return Ok(false);
    }

    let revived = transaction.execute(
        r#"
        UPDATE sync_outbox
        SET
          resource = ?1,
          operation = ?2,
          object_id = ?3,
          payload_json = ?4,
          status = 'pending',
          attempt_count = 0,
          next_attempt_epoch_ms = 0,
          last_error = NULL,
          updated_at = CURRENT_TIMESTAMP
        WHERE rowid = (
          SELECT rowid
          FROM sync_outbox
          WHERE resource = ?1
            AND operation = ?2
            AND object_id = ?3
            AND status NOT IN ('pending', 'processing')
          ORDER BY updated_at DESC, rowid DESC
          LIMIT 1
        )
        "#,
        params![resource, operation, object_id, payload_json],
    )?;

    let inserted = if revived == 0 {
        transaction.execute(
            r#"
            INSERT INTO sync_outbox (
              id, resource, operation, object_id, payload_json,
              status, attempt_count, next_attempt_epoch_ms, last_error, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, 0, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(id) DO UPDATE SET
              resource = excluded.resource,
              operation = excluded.operation,
              object_id = excluded.object_id,
              payload_json = excluded.payload_json,
              status = 'pending',
              attempt_count = 0,
              next_attempt_epoch_ms = excluded.next_attempt_epoch_ms,
              last_error = NULL,
              updated_at = CURRENT_TIMESTAMP
            WHERE sync_outbox.status NOT IN ('pending', 'processing')
            "#,
            params![id, resource, operation, object_id, payload_json],
        )?
    } else {
        0
    };
    Ok(revived > 0 || inserted > 0)
}

impl Store {
    pub fn enqueue_outbox_item_if_not_pending(
        &self,
        resource: &str,
        operation: &str,
        object_id: &str,
        payload_json: &str,
    ) -> StoreResult<bool> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let queued = enqueue_outbox_item_if_not_pending_in_transaction(
            &transaction,
            resource,
            operation,
            object_id,
            payload_json,
        )?;
        transaction.commit()?;
        Ok(queued)
    }

    pub fn upsert_journal_and_enqueue_update(
        &self,
        journal_id: &str,
        updated_at: Option<&str>,
        deleted_at: Option<&str>,
        data_json: &str,
    ) -> StoreResult<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            r#"
            INSERT INTO journals (id, updated_at, deleted_at, is_deleted, data_json)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
              updated_at = excluded.updated_at,
              deleted_at = excluded.deleted_at,
              is_deleted = excluded.is_deleted,
              data_json = excluded.data_json
            "#,
            params![
                journal_id,
                updated_at,
                deleted_at,
                if deleted_at.is_some() { 1 } else { 0 },
                data_json
            ],
        )?;
        let marker = format!(
            r#"{{"data_hash":"{:x}"}}"#,
            Sha256::digest(data_json.as_bytes())
        );
        let queued = enqueue_outbox_item_if_not_pending_in_transaction(
            &transaction,
            "journal",
            "update",
            journal_id,
            &marker,
        )?;
        if !queued {
            transaction.execute(
                r#"
                UPDATE sync_outbox
                SET payload_json = ?2,
                    updated_at = CURRENT_TIMESTAMP
                WHERE resource = 'journal'
                  AND operation = 'update'
                  AND object_id = ?1
                  AND status IN ('pending', 'processing')
                "#,
                params![journal_id, marker],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn apply_journal_update_response_if_current(
        &self,
        outbox_id: &str,
        leased_payload_json: &str,
        journal_id: &str,
        updated_at: Option<&str>,
        deleted_at: Option<&str>,
        data_json: &str,
    ) -> StoreResult<bool> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let lease_is_current: bool = transaction.query_row(
            r#"
            SELECT EXISTS(
              SELECT 1
              FROM sync_outbox
              WHERE id = ?1
                AND resource = 'journal'
                AND operation = 'update'
                AND object_id = ?2
                AND status = 'processing'
                AND payload_json = ?3
            )
            "#,
            params![outbox_id, journal_id, leased_payload_json],
            |row| row.get(0),
        )?;
        if !lease_is_current {
            transaction.commit()?;
            return Ok(false);
        }

        transaction.execute(
            r#"
            INSERT INTO journals (id, updated_at, deleted_at, is_deleted, data_json)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
              updated_at = excluded.updated_at,
              deleted_at = excluded.deleted_at,
              is_deleted = excluded.is_deleted,
              data_json = excluded.data_json
            "#,
            params![
                journal_id,
                updated_at,
                deleted_at,
                if deleted_at.is_some() { 1 } else { 0 },
                data_json
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn enqueue_outbox_item(
        &self,
        id: &str,
        resource: &str,
        operation: &str,
        object_id: &str,
        payload_json: &str,
        next_attempt_epoch_ms: i64,
    ) -> StoreResult<()> {
        self.enqueue_outbox_item_with_policy(
            id,
            resource,
            operation,
            object_id,
            payload_json,
            next_attempt_epoch_ms,
            true,
        )
        .map(|_| ())
    }

    pub(crate) fn enqueue_outbox_item_from_pull(
        &self,
        id: &str,
        resource: &str,
        operation: &str,
        object_id: &str,
        payload_json: &str,
        next_attempt_epoch_ms: i64,
    ) -> StoreResult<bool> {
        self.enqueue_outbox_item_with_policy(
            id,
            resource,
            operation,
            object_id,
            payload_json,
            next_attempt_epoch_ms,
            false,
        )
    }

    /// Requeue without changing the payload or erasing its previous failure.
    /// Pull reconciliation must preserve recovery work until an upload settles it.
    pub fn retry_failed_outbox_item(&self, id: &str) -> StoreResult<bool> {
        let conn = self.connect()?;
        Ok(conn.execute(
            "UPDATE sync_outbox SET status = 'pending', attempt_count = 0, next_attempt_epoch_ms = 0, last_error = COALESCE(last_error, 'previous failure; manual retry requested'), updated_at = CURRENT_TIMESTAMP WHERE id = ?1 AND status = 'failed'",
            params![id],
        )? == 1)
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_outbox_item_with_policy(
        &self,
        id: &str,
        resource: &str,
        operation: &str,
        object_id: &str,
        payload_json: &str,
        next_attempt_epoch_ms: i64,
        retry_failed: bool,
    ) -> StoreResult<bool> {
        let conn = self.connect()?;
        let changed = conn.execute(
            r#"
            INSERT INTO sync_outbox (
              id, resource, operation, object_id, payload_json,
              status, attempt_count, next_attempt_epoch_ms, last_error, created_at, updated_at
            )
            SELECT ?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
            WHERE ?7 OR NOT EXISTS (
              SELECT 1 FROM sync_outbox
              WHERE resource = ?2 AND object_id = ?4
                AND (status = 'failed' OR last_error IS NOT NULL)
            )
            ON CONFLICT(id) DO UPDATE SET
              resource = excluded.resource,
              operation = excluded.operation,
              object_id = excluded.object_id,
              payload_json = excluded.payload_json,
              status = CASE
                WHEN sync_outbox.status = 'processing' THEN 'processing'
                ELSE 'pending'
              END,
              attempt_count = CASE
                WHEN sync_outbox.status = 'processing' THEN sync_outbox.attempt_count
                ELSE 0
              END,
              next_attempt_epoch_ms = CASE
                WHEN sync_outbox.status = 'processing' THEN sync_outbox.next_attempt_epoch_ms
                ELSE excluded.next_attempt_epoch_ms
              END,
              last_error = CASE
                WHEN sync_outbox.status = 'processing' THEN sync_outbox.last_error
                ELSE NULL
              END,
              updated_at = CURRENT_TIMESTAMP
            WHERE ?7 OR sync_outbox.status != 'failed'
            "#,
            params![
                id,
                resource,
                operation,
                object_id,
                payload_json,
                next_attempt_epoch_ms,
                retry_failed
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn list_outbox_item_details(
        &self,
        include_payload: bool,
    ) -> StoreResult<Vec<OutboxItemDetails>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, resource, operation, object_id, status,
                   attempt_count, next_attempt_epoch_ms, last_error, created_at, updated_at,
                   CASE WHEN ?1 THEN payload_json ELSE NULL END
            FROM sync_outbox
            ORDER BY created_at ASC, id ASC
            "#,
        )?;
        let rows = stmt.query_map(params![include_payload], |row| {
            Ok(OutboxItemDetails {
                id: row.get(0)?,
                resource: row.get(1)?,
                operation: row.get(2)?,
                object_id: row.get(3)?,
                status: row.get(4)?,
                attempt_count: row.get(5)?,
                next_attempt_epoch_ms: row.get(6)?,
                last_error: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                payload_json: row.get(10)?,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn delete_outbox_item_by_id(&self, id: &str) -> StoreResult<usize> {
        let conn = self.connect()?;
        Ok(conn.execute("DELETE FROM sync_outbox WHERE id = ?1", params![id])?)
    }

    pub fn delete_outbox_items_by_status(&self, status: &str) -> StoreResult<usize> {
        let conn = self.connect()?;
        Ok(conn.execute("DELETE FROM sync_outbox WHERE status = ?1", params![status])?)
    }

    pub fn delete_all_outbox_items(&self) -> StoreResult<usize> {
        let conn = self.connect()?;
        Ok(conn.execute("DELETE FROM sync_outbox", [])?)
    }

    pub fn lease_outbox_items(
        &self,
        now_epoch_ms: i64,
        limit: usize,
    ) -> StoreResult<Vec<OutboxItem>> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut candidates = Vec::new();
        {
            let mut select = tx.prepare(
                r#"
                SELECT o.id, o.resource, o.operation, o.object_id, o.payload_json, o.attempt_count
                FROM sync_outbox o
                WHERE o.status = 'pending'
                  AND o.next_attempt_epoch_ms <= ?1
                  AND (
                    o.resource != 'original_media'
                    OR EXISTS (
                      SELECT 1
                      FROM entry_attachments a
                      WHERE a.journal_id = json_extract(o.payload_json, '$.journal_id')
                        AND a.entry_id = json_extract(o.payload_json, '$.entry_id')
                        AND a.moment_id = json_extract(o.payload_json, '$.moment_id')
                        AND a.entry_associated = 1
                    )
                  )
                  AND (
                    NOT EXISTS (
                      SELECT 1
                      FROM sync_outbox j
                      WHERE j.status IN ('pending', 'processing')
                        AND j.resource = 'journal'
                        AND j.operation = 'create'
                        AND (
                          (o.resource = 'entry' AND (o.object_id = j.object_id OR instr(o.object_id, j.object_id || ':') = 1))
                          OR (o.resource = 'journal' AND o.operation = 'update' AND o.object_id = j.object_id)
                        )
                    )
                    OR (o.resource = 'journal' AND o.operation = 'create')
                  )
                ORDER BY
                  CASE WHEN o.resource = 'journal' AND o.operation = 'create' THEN 0 ELSE 1 END,
                  o.next_attempt_epoch_ms ASC,
                  o.created_at ASC,
                  o.id ASC
                LIMIT ?2
                "#,
            )?;
            let rows = select.query_map(params![now_epoch_ms, limit as i64], |row| {
                Ok(OutboxItem {
                    id: row.get(0)?,
                    resource: row.get(1)?,
                    operation: row.get(2)?,
                    object_id: row.get(3)?,
                    payload_json: row.get(4)?,
                    attempt_count: row.get(5)?,
                })
            })?;
            for row in rows {
                candidates.push(row?);
            }
        }
        let mut items = Vec::with_capacity(candidates.len());
        for item in candidates {
            let leased = tx.execute(
                "UPDATE sync_outbox SET status = 'processing', updated_at = CURRENT_TIMESTAMP WHERE id = ?1 AND status = 'pending'",
                params![item.id],
            )?;
            if leased == 1 {
                items.push(item);
            }
        }
        tx.commit()?;
        Ok(items)
    }

    pub fn mark_outbox_item_succeeded(
        &self,
        id: &str,
        leased_payload_json: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        let deleted = conn.execute(
            "DELETE FROM sync_outbox WHERE id = ?1 AND status = 'processing' AND payload_json = ?2",
            params![id, leased_payload_json],
        )?;
        if deleted == 0 {
            conn.execute(
                r#"
                UPDATE sync_outbox
                SET status = 'pending',
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?1 AND status = 'processing'
                "#,
                params![id],
            )?;
        }
        Ok(())
    }

    /// Whether a local entry update is still queued or currently being pushed.
    /// Pull conflict resolution uses this to distinguish a real local edit from
    /// a server-origin row whose timestamp merely ties the incoming revision.
    pub fn has_inflight_entry_update(&self, journal_id: &str, entry_id: &str) -> StoreResult<bool> {
        let conn = self.connect()?;
        Ok(conn.query_row(
            r#"
            SELECT EXISTS(
              SELECT 1
              FROM sync_outbox
              WHERE resource = 'entry'
                AND operation = 'update'
                AND object_id = ?1
                AND status IN ('pending', 'processing')
            )
            "#,
            params![format!("{journal_id}:{entry_id}")],
            |row| row.get(0),
        )?)
    }

    /// Whether any outbox row references this entry, in any status and under
    /// any journal (entries moved between journals keep their entry id). Entry
    /// rows use `object_id = "<journal>:<entry>"` while media and comment rows
    /// use `"<journal>:<entry>:<child>"`, so the entry id is always the second
    /// path segment; matching that segment exactly avoids false positives when
    /// a child id happens to equal the entry id.
    pub fn has_outbox_evidence_for_entry(&self, entry_id: &str) -> StoreResult<bool> {
        let conn = self.connect()?;
        Ok(conn.query_row(
            r#"
            SELECT EXISTS(
              SELECT 1
              FROM sync_outbox
              WHERE instr(object_id, ':') > 0
                AND (
                  substr(object_id, instr(object_id, ':') + 1) = ?1
                  OR instr(substr(object_id, instr(object_id, ':') + 1), ?1 || ':') = 1
                )
            )
            "#,
            params![entry_id],
            |row| row.get(0),
        )?)
    }

    /// Clear settled work, but retain failed uploads and their pending retries
    /// until an upload succeeds or the user explicitly clears the recovery record.
    pub fn delete_settled_entry_outbox_for_entry(
        &self,
        journal_id: &str,
        entry_id: &str,
    ) -> StoreResult<usize> {
        let conn = self.connect()?;
        let deleted = conn.execute(
            r#"
            DELETE FROM sync_outbox
            WHERE resource = 'entry'
              AND object_id = ?1
              AND status != 'failed' AND last_error IS NULL
            "#,
            params![format!("{journal_id}:{entry_id}")],
        )?;
        Ok(deleted)
    }

    pub fn delete_original_media_outbox_for_entry(
        &self,
        journal_id: &str,
        entry_id: &str,
    ) -> StoreResult<usize> {
        let conn = self.connect()?;
        // Literal prefix match (no LIKE), so '%' and '_' in IDs are treated as normal chars.
        let object_prefix = format!("{journal_id}:{entry_id}:");
        let deleted = conn.execute(
            r#"
            DELETE FROM sync_outbox
            WHERE resource = 'original_media'
              AND operation = 'create'
              AND instr(object_id, ?1) = 1
            "#,
            params![object_prefix],
        )?;
        Ok(deleted)
    }

    pub fn mark_outbox_item_retry(
        &self,
        id: &str,
        leased_payload_json: &str,
        next_attempt_epoch_ms: i64,
        error: Option<&str>,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        let updated = conn.execute(
            r#"
            UPDATE sync_outbox
            SET status = 'pending',
                attempt_count = attempt_count + 1,
                next_attempt_epoch_ms = ?3,
                last_error = ?4,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1 AND status = 'processing' AND payload_json = ?2
            "#,
            params![id, leased_payload_json, next_attempt_epoch_ms, error],
        )?;
        if updated == 0 {
            conn.execute(
                r#"
                UPDATE sync_outbox
                SET status = 'pending',
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?1 AND status = 'processing'
                "#,
                params![id],
            )?;
        }
        Ok(())
    }

    pub fn mark_outbox_item_deferred(
        &self,
        id: &str,
        leased_payload_json: &str,
        next_attempt_epoch_ms: i64,
        error: Option<&str>,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        let updated = conn.execute(
            r#"
            UPDATE sync_outbox
            SET status = 'pending',
                next_attempt_epoch_ms = ?3,
                last_error = ?4,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1 AND status = 'processing' AND payload_json = ?2
            "#,
            params![id, leased_payload_json, next_attempt_epoch_ms, error],
        )?;
        if updated == 0 {
            conn.execute(
                r#"
                UPDATE sync_outbox
                SET status = 'pending',
                    next_attempt_epoch_ms = ?2,
                    last_error = ?3,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?1 AND status = 'processing'
                "#,
                params![id, next_attempt_epoch_ms, error],
            )?;
        }
        Ok(())
    }

    pub fn mark_outbox_item_failed(
        &self,
        id: &str,
        leased_payload_json: &str,
        final_attempt_count: i64,
        error: Option<&str>,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        let updated = conn.execute(
            r#"
            UPDATE sync_outbox
            SET status = 'failed',
                attempt_count = ?3,
                last_error = ?4,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1 AND status = 'processing' AND payload_json = ?2
            "#,
            params![id, leased_payload_json, final_attempt_count, error],
        )?;
        if updated == 0 {
            conn.execute(
                r#"
                UPDATE sync_outbox
                SET status = 'pending',
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?1 AND status = 'processing'
                "#,
                params![id],
            )?;
        }
        Ok(())
    }

    pub fn reset_processing_outbox_items(&self, stale_before_epoch_ms: i64) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            UPDATE sync_outbox
            SET status = 'pending', updated_at = CURRENT_TIMESTAMP
            WHERE status = 'processing'
              AND (CAST(strftime('%s', updated_at) AS INTEGER) * 1000) <= ?1
            "#,
            params![stale_before_epoch_ms],
        )?;
        Ok(())
    }

    pub fn reset_processing_outbox_items_by_ids(&self, ids: &[String]) -> StoreResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.connect()?;
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE sync_outbox
             SET status = 'pending', updated_at = CURRENT_TIMESTAMP
             WHERE status = 'processing' AND id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params = rusqlite::params_from_iter(ids.iter());
        stmt.execute(params)?;
        Ok(())
    }
}
