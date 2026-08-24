use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, params};

use crate::store::StoreResult;
use crate::store::sqlite::{JournalSyncStatus, Store};

pub(crate) type JournalMetadataDiagnostic<'a> = (&'a str, &'a str);
#[cfg(test)]
pub(crate) type OwnedJournalMetadataDiagnostic = (String, String);

/// Arguments for [`Store::save_sync_resource_run`], grouped to satisfy clippy's argument count lint.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SyncResourceRunSave<'a> {
    pub run_id: i64,
    pub resource_key: &'a str,
    pub changed_count: i64,
    pub cursor_before: Option<&'a str>,
    pub cursor_after: Option<&'a str>,
    pub status: &'a str,
    pub error: Option<&'a str>,
}

pub(crate) fn now_epoch_ms() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

fn validate_journal_metadata_diagnostics(
    diagnostics: &[JournalMetadataDiagnostic<'_>],
) -> StoreResult<()> {
    if diagnostics.iter().any(|(field, status)| {
        !matches!(*field, "name" | "description_v2")
            || !matches!(
                *status,
                "plaintext" | "malformed_d1" | "undecryptable_d1" | "unverified_d1"
            )
    }) {
        return Err(crate::store::StoreError::invalid_input(
            "invalid journal metadata diagnostic",
        ));
    }
    Ok(())
}

fn replace_journal_metadata_diagnostics_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    journal_id: &str,
    diagnostics: &[JournalMetadataDiagnostic<'_>],
) -> StoreResult<()> {
    transaction.execute(
        "DELETE FROM journal_metadata_diagnostics WHERE journal_id = ?1",
        params![journal_id],
    )?;
    for (field, status) in diagnostics {
        transaction.execute(
            "INSERT INTO journal_metadata_diagnostics (journal_id, field, status) VALUES (?1, ?2, ?3)",
            params![journal_id, field, status],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn get_sync_cursor(&self, resource_key: &str) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT cursor FROM sync_cursors WHERE resource_key = ?1",
            params![resource_key],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|v| v.flatten())
        .map_err(Into::into)
    }

    pub fn set_sync_cursor(&self, resource_key: &str, cursor: Option<&str>) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO sync_cursors (resource_key, cursor, updated_at)
            VALUES (?1, ?2, CURRENT_TIMESTAMP)
            ON CONFLICT(resource_key) DO UPDATE SET
              cursor = excluded.cursor,
              updated_at = CURRENT_TIMESTAMP
            "#,
            params![resource_key, cursor],
        )?;
        Ok(())
    }

    pub fn clear_sync_cursors(&self) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM sync_cursors", [])?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn journal_metadata_inspection_is_pending(&self) -> StoreResult<bool> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM journal_metadata_inspection WHERE id = 1)",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub(crate) fn request_journal_metadata_inspection(&self) -> StoreResult<()> {
        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO journal_metadata_inspection (id, state) VALUES (1, 'requested') ON CONFLICT(id) DO NOTHING",
            [],
        )?;
        Ok(())
    }

    pub(crate) fn start_journal_metadata_inspection(&self) -> StoreResult<bool> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let state = transaction
            .query_row(
                "SELECT state FROM journal_metadata_inspection WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if state.as_deref() != Some("requested") {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            r#"
            INSERT INTO sync_cursors (resource_key, cursor, updated_at)
            VALUES ('journals', NULL, CURRENT_TIMESTAMP)
            ON CONFLICT(resource_key) DO UPDATE SET
              cursor = NULL,
              updated_at = CURRENT_TIMESTAMP
            "#,
            [],
        )?;
        transaction.execute(
            "UPDATE journal_metadata_inspection SET state = 'running' WHERE id = 1",
            [],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn complete_journal_metadata_inspection(&self) -> StoreResult<bool> {
        let conn = self.connect()?;
        Ok(conn.execute(
            "DELETE FROM journal_metadata_inspection WHERE id = 1 AND state = 'running'",
            [],
        )? != 0)
    }

    pub(crate) fn journals_with_unverified_metadata(&self) -> StoreResult<Vec<String>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT journal_id FROM journal_metadata_diagnostics WHERE status = 'unverified_d1' ORDER BY journal_id",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn journal_metadata_diagnostics(
        &self,
        journal_id: &str,
    ) -> StoreResult<Vec<OwnedJournalMetadataDiagnostic>> {
        let conn = self.connect()?;
        let mut statement = conn.prepare(
            "SELECT field, status FROM journal_metadata_diagnostics WHERE journal_id = ?1 ORDER BY field",
        )?;
        statement
            .query_map([journal_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn replace_journal_metadata_diagnostics(
        &self,
        journal_id: &str,
        diagnostics: &[JournalMetadataDiagnostic<'_>],
    ) -> StoreResult<()> {
        validate_journal_metadata_diagnostics(diagnostics)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        replace_journal_metadata_diagnostics_in_transaction(&transaction, journal_id, diagnostics)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn apply_pulled_journal(
        &self,
        journal_id: &str,
        updated_at: Option<&str>,
        deleted_at: Option<&str>,
        data_json: &str,
        observed_fields: &[&str],
        diagnostics: &[JournalMetadataDiagnostic<'_>],
    ) -> StoreResult<bool> {
        validate_journal_metadata_diagnostics(diagnostics)?;
        if observed_fields
            .iter()
            .any(|field| !matches!(*field, "name" | "description_v2"))
        {
            return Err(crate::store::StoreError::invalid_input(
                "invalid journal metadata field",
            ));
        }

        let mut connection = self.connect()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let update_inflight: bool = transaction.query_row(
            r#"
            SELECT EXISTS(
              SELECT 1
              FROM sync_outbox
              WHERE resource = 'journal'
                AND operation = 'update'
                AND object_id = ?1
                AND status IN ('pending', 'processing')
            )
            "#,
            params![journal_id],
            |row| row.get(0),
        )?;

        if update_inflight {
            for field in observed_fields {
                transaction.execute(
                    "DELETE FROM journal_metadata_diagnostics WHERE journal_id = ?1 AND field = ?2",
                    params![journal_id, field],
                )?;
            }
            for (field, status) in diagnostics {
                transaction.execute(
                    r#"
                    INSERT INTO journal_metadata_diagnostics (journal_id, field, status)
                    VALUES (?1, ?2, ?3)
                    ON CONFLICT(journal_id, field) DO UPDATE SET status = excluded.status
                    "#,
                    params![journal_id, field, status],
                )?;
            }
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
        replace_journal_metadata_diagnostics_in_transaction(&transaction, journal_id, diagnostics)?;
        transaction.execute(
            r#"
            INSERT INTO journal_sync_info (
              journal_id, status, has_completed, last_selected_at_epoch_ms
            ) VALUES (?1, ?2, 0, ?3)
            ON CONFLICT(journal_id) DO UPDATE SET
              status = excluded.status,
              has_completed = excluded.has_completed,
              last_selected_at_epoch_ms = excluded.last_selected_at_epoch_ms
            "#,
            params![
                journal_id,
                JournalSyncStatus::WaitingInitialSync.as_str(),
                now_epoch_ms()
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn try_acquire_sync_lock(&self, ttl_ms: i64) -> StoreResult<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let now_ms = now_epoch_ms();
        let existing_lock: Option<i64> = tx
            .query_row(
                "SELECT locked_at_epoch_ms FROM sync_lock WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;

        let is_stale = existing_lock
            .map(|locked_at| now_ms.saturating_sub(locked_at) > ttl_ms)
            .unwrap_or(true);

        if !is_stale {
            return Ok(false);
        }

        tx.execute(
            r#"
            INSERT INTO sync_lock (id, locked_at_epoch_ms)
            VALUES (1, ?1)
            ON CONFLICT(id) DO UPDATE SET locked_at_epoch_ms = excluded.locked_at_epoch_ms
            "#,
            params![now_ms],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn release_sync_lock(&self) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM sync_lock WHERE id = 1", [])?;
        Ok(())
    }

    pub fn set_journal_sync_status(
        &self,
        journal_id: &str,
        status: JournalSyncStatus,
        has_completed: bool,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO journal_sync_info (journal_id, status, has_completed, last_selected_at_epoch_ms)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(journal_id) DO UPDATE SET
              status = excluded.status,
              has_completed = excluded.has_completed,
              last_selected_at_epoch_ms = excluded.last_selected_at_epoch_ms
            "#,
            params![
                journal_id,
                status.as_str(),
                if has_completed { 1 } else { 0 },
                now_epoch_ms()
            ],
        )?;
        Ok(())
    }

    pub fn enable_journal_sync(&self, journal_id: &str) -> StoreResult<()> {
        self.set_journal_sync_status(journal_id, JournalSyncStatus::WaitingInitialSync, false)
    }

    pub fn get_sync_enabled_journal_ids(&self) -> StoreResult<Vec<String>> {
        let conn = self.connect()?;
        let mut stmt =
            conn.prepare("SELECT journal_id FROM journal_sync_info ORDER BY journal_id ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    pub fn delete_journal_sync_info(&self, journal_id: &str) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM journal_sync_info WHERE journal_id = ?1",
            params![journal_id],
        )?;
        Ok(())
    }

    pub fn set_sync_resource_state_success(&self, resource_key: &str) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO sync_resource_state (resource_key, last_successful_sync_at, last_error)
            VALUES (?1, CURRENT_TIMESTAMP, NULL)
            ON CONFLICT(resource_key) DO UPDATE SET
              last_successful_sync_at = CURRENT_TIMESTAMP,
              last_error = NULL
            "#,
            params![resource_key],
        )?;
        Ok(())
    }

    pub fn set_sync_resource_state_error(
        &self,
        resource_key: &str,
        error: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO sync_resource_state (resource_key, last_successful_sync_at, last_error)
            VALUES (?1, NULL, ?2)
            ON CONFLICT(resource_key) DO UPDATE SET
              last_error = excluded.last_error
            "#,
            params![resource_key, error],
        )?;
        Ok(())
    }

    pub fn create_sync_run(&self) -> StoreResult<i64> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO sync_runs (status, started_at) VALUES ('running', CURRENT_TIMESTAMP)",
            [],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_sync_run(
        &self,
        run_id: i64,
        ok: bool,
        fatal_error: Option<&str>,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE sync_runs SET status = ?2, finished_at = CURRENT_TIMESTAMP, fatal_error = ?3 WHERE id = ?1",
            params![run_id, if ok { "success" } else { "failed" }, fatal_error],
        )?;
        Ok(())
    }

    /// Seconds since the most recent successful sync run finished, or `None` if
    /// there has never been a completed successful sync. Used to gate operations
    /// that require reasonably fresh local state.
    #[expect(
        dead_code,
        reason = "doctor reads sync age through its read-only diagnostic connection"
    )]
    pub fn seconds_since_last_successful_sync(&self) -> StoreResult<Option<i64>> {
        let conn = self.connect()?;
        let secs = conn.query_row(
            "SELECT CAST(strftime('%s', 'now') AS INTEGER) \
                  - CAST(strftime('%s', MAX(finished_at)) AS INTEGER) \
             FROM sync_runs \
             WHERE status = 'success' AND finished_at IS NOT NULL",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(secs)
    }

    pub fn save_sync_resource_run(&self, args: SyncResourceRunSave<'_>) -> StoreResult<()> {
        let conn = self.connect()?;
        let cursor_advanced = match (args.cursor_before, args.cursor_after) {
            (Some(before), Some(after)) => before != after,
            (None, Some(after)) => !after.is_empty(),
            _ => false,
        };
        conn.execute(
            r#"
            INSERT INTO sync_resource_runs (
              run_id, resource_key, started_at, finished_at, status, changed_count,
              cursor_before, cursor_after, cursor_advanced, error
            )
            VALUES (?1, ?2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                args.run_id,
                args.resource_key,
                args.status,
                args.changed_count,
                args.cursor_before,
                args.cursor_after,
                if cursor_advanced { 1 } else { 0 },
                args.error
            ],
        )?;
        Ok(())
    }
}
