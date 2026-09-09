use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rusqlite::{OptionalExtension, named_params, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::sqlite::{ListJsonTable, ListPageRequest, ListPageResult, Store};
use crate::store::{StoreError, StoreResult};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ListCursorPayload {
    pub(crate) updated_at: String,
    pub(crate) id: String,
}

pub(crate) fn encode_list_cursor(updated_at: &str, id: &str) -> String {
    let payload = serde_json::json!({
        "updated_at": updated_at,
        "id": id,
    });
    BASE64_STANDARD.encode(payload.to_string())
}

pub(crate) fn decode_list_cursor(cursor: Option<&str>) -> StoreResult<Option<ListCursorPayload>> {
    let Some(raw) = cursor else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(StoreError::invalid_input("cursor cannot be empty"));
    }
    let decoded = BASE64_STANDARD.decode(trimmed).map_err(|_| {
        StoreError::invalid_input("cursor is invalid: expected base64-encoded token")
    })?;
    let payload: ListCursorPayload = serde_json::from_slice(&decoded)
        .map_err(|_| StoreError::invalid_input("cursor is invalid: expected encoded JSON token"))?;
    if payload.id.trim().is_empty() {
        return Err(StoreError::invalid_input("cursor is invalid: missing id"));
    }
    Ok(Some(payload))
}

pub(crate) fn sqlite_page_params(page: &ListPageRequest) -> StoreResult<(i64, i64)> {
    if page.offset.is_some() && page.cursor.is_some() {
        return Err(StoreError::invalid_input(
            "pagination offset and cursor cannot be used together",
        ));
    }
    let offset = i64::try_from(page.offset.unwrap_or(0))
        .map_err(|_| StoreError::invalid_input("pagination offset is too large for sqlite"))?;
    let limit_plus_one = match page.limit {
        Some(limit) => {
            if limit == 0 {
                return Err(StoreError::invalid_input(
                    "pagination limit must be at least 1",
                ));
            }
            let limit = i64::try_from(limit).map_err(|_| {
                StoreError::invalid_input("pagination limit is too large for sqlite")
            })?;
            limit.checked_add(1).ok_or_else(|| {
                StoreError::invalid_input("pagination limit is too large for sqlite")
            })?
        }
        None => -1,
    };
    Ok((offset, limit_plus_one))
}

pub(crate) fn build_page_result(
    mut items: Vec<(String, String, String)>,
    limit: Option<usize>,
) -> StoreResult<ListPageResult> {
    let mut has_more = false;
    if let Some(limit) = limit
        && items.len() > limit
    {
        has_more = true;
        items.truncate(limit);
    }
    let next_cursor = if has_more {
        items
            .last()
            .map(|(_, updated_at_key, id)| encode_list_cursor(updated_at_key, id))
    } else {
        None
    };
    Ok(ListPageResult {
        rows: items.into_iter().map(|(row, _, _)| row).collect(),
        has_more,
        next_cursor,
    })
}

impl Store {
    pub fn upsert_json_row(
        &self,
        table: &str,
        id: &str,
        updated_at: Option<&str>,
        deleted_at: Option<&str>,
        data_json: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        let sql = format!(
            r#"
            INSERT INTO {table} (id, updated_at, deleted_at, is_deleted, data_json)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
              updated_at = excluded.updated_at,
              deleted_at = excluded.deleted_at,
              is_deleted = excluded.is_deleted,
              data_json = excluded.data_json
            "#
        );
        conn.execute(
            &sql,
            params![
                id,
                updated_at,
                deleted_at,
                if deleted_at.is_some() { 1 } else { 0 },
                data_json
            ],
        )?;
        Ok(())
    }

    pub fn upsert_json_row_with_sync_state(
        &self,
        table: &str,
        id: &str,
        updated_at: Option<&str>,
        deleted_at: Option<&str>,
        needs_sync: Option<bool>,
        data_json: &str,
    ) -> StoreResult<()> {
        if needs_sync.is_none() {
            return self.upsert_json_row(table, id, updated_at, deleted_at, data_json);
        }
        let conn = self.connect()?;
        let sql = format!(
            r#"
            INSERT INTO {table} (id, updated_at, deleted_at, is_deleted, needs_sync, data_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(id) DO UPDATE SET
              updated_at = excluded.updated_at,
              deleted_at = excluded.deleted_at,
              is_deleted = excluded.is_deleted,
              needs_sync = excluded.needs_sync,
              data_json = excluded.data_json
            "#
        );
        conn.execute(
            &sql,
            params![
                id,
                updated_at,
                deleted_at,
                if deleted_at.is_some() { 1 } else { 0 },
                if needs_sync.unwrap_or(false) { 1 } else { 0 },
                data_json
            ],
        )?;
        Ok(())
    }

    pub fn upsert_singleton_json_row(
        &self,
        table: &str,
        id: i64,
        data_json: &str,
        updated_at: Option<&str>,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        let sql = format!(
            r#"
            INSERT INTO {table} (id, data_json, updated_at)
            VALUES (?1, ?2, COALESCE(?3, CURRENT_TIMESTAMP))
            ON CONFLICT(id) DO UPDATE SET
              data_json = excluded.data_json,
              updated_at = COALESCE(excluded.updated_at, CURRENT_TIMESTAMP)
            "#
        );
        conn.execute(&sql, params![id, data_json, updated_at])?;
        Ok(())
    }

    pub fn upsert_user_settings(&self, cursor: Option<&str>, data_json: &str) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO user_settings (id, cursor, data_json, updated_at)
            VALUES (1, ?1, ?2, CURRENT_TIMESTAMP)
            ON CONFLICT(id) DO UPDATE SET
              cursor = excluded.cursor,
              data_json = excluded.data_json,
              updated_at = CURRENT_TIMESTAMP
            "#,
            params![cursor, data_json],
        )?;
        Ok(())
    }

    pub fn get_json_row_by_id(&self, table: &str, id: &str) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        let sql = format!("SELECT data_json FROM {table} WHERE id = ?1 LIMIT 1");
        conn.query_row(&sql, params![id], |row| row.get::<_, String>(0))
            .optional()
            .map_err(Into::into)
    }

    /// Returns the authoritative `/users/key` payload. The legacy fallback
    /// keeps upgraded profiles usable until their next sync populates the
    /// dedicated table.
    pub fn get_user_identity_key_json(&self) -> StoreResult<Option<String>> {
        if let Some(user_key) = self.get_json_row_by_id("user_identity_key", "1")? {
            return Ok(Some(user_key));
        }
        self.get_json_row_by_id("user_keys", "1")
    }

    pub fn get_daily_chat_row_by_date(&self, date: &str) -> StoreResult<Option<(String, String)>> {
        let conn = self.connect()?;
        conn.query_row(
            r#"
            SELECT id, data_json
            FROM daily_chat_feed
            WHERE json_extract(data_json, '$.date') = ?1
            ORDER BY updated_at DESC, id DESC
            LIMIT 1
            "#,
            params![date],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_json_row_needs_sync(&self, table: &str, id: &str) -> StoreResult<Option<bool>> {
        let conn = self.connect()?;
        let sql = format!("SELECT needs_sync FROM {table} WHERE id = ?1 LIMIT 1");
        conn.query_row(&sql, params![id], |row| row.get::<_, i64>(0))
            .optional()
            .map(|v| v.map(|flag| flag != 0))
            .map_err(Into::into)
    }

    pub fn delete_json_row_by_id(&self, table: &str, id: &str) -> StoreResult<()> {
        let conn = self.connect()?;
        let sql = format!("DELETE FROM {table} WHERE id = ?1");
        conn.execute(&sql, params![id])?;
        Ok(())
    }

    pub fn journal_row_count(&self) -> StoreResult<i64> {
        let conn = self.connect()?;
        conn.query_row("SELECT COUNT(*) FROM journals", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn has_active_local_journal(&self, journal_id: &str) -> StoreResult<bool> {
        let conn = self.connect()?;
        let row = conn
            .query_row(
                "SELECT deleted_at, is_deleted, data_json FROM journals WHERE id = ?1 LIMIT 1",
                params![journal_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((deleted_at, is_deleted, data_json)) = row else {
            return Ok(false);
        };
        if deleted_at.is_some() || is_deleted != 0 {
            return Ok(false);
        }
        let value: Value = serde_json::from_str(&data_json)?;
        Ok(value
            .get("state")
            .and_then(Value::as_str)
            .is_none_or(|state| state != "deleted"))
    }

    pub fn delete_local_journal(&self, journal_id: &str) -> StoreResult<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM comment_reactions WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute(
            "DELETE FROM comments WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute(
            "DELETE FROM entry_attachments WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute(
            "DELETE FROM entry_embeddings WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute(
            "DELETE FROM entry_embeddings_vec WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute(
            "DELETE FROM unread_entries WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute(
            "DELETE FROM entries WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute("DELETE FROM journals WHERE id = ?1", params![journal_id])?;
        tx.execute(
            "DELETE FROM journal_metadata_diagnostics WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute(
            "DELETE FROM journal_sync_info WHERE journal_id = ?1",
            params![journal_id],
        )?;
        tx.execute(
            r#"
            DELETE FROM sync_cursors
            WHERE resource_key = ?1
               OR instr(resource_key, ?2) = 1
            "#,
            params![
                format!("entries:{journal_id}"),
                format!("comments:{journal_id}:")
            ],
        )?;
        tx.execute(
            r#"
            DELETE FROM sync_outbox
            WHERE status != 'failed' AND last_error IS NULL AND (
              (resource = 'journal' AND object_id = ?1)
              OR (resource IN ('entry', 'original_media', 'comment') AND instr(object_id, ?1 || ':') = 1)
            )
            "#,
            params![journal_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_local_entry(&self, journal_id: &str, entry_id: &str) -> StoreResult<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM comment_reactions WHERE journal_id = ?1 AND entry_id = ?2",
            params![journal_id, entry_id],
        )?;
        tx.execute(
            "DELETE FROM comments WHERE journal_id = ?1 AND entry_id = ?2",
            params![journal_id, entry_id],
        )?;
        tx.execute(
            "DELETE FROM entry_attachments WHERE journal_id = ?1 AND entry_id = ?2",
            params![journal_id, entry_id],
        )?;
        tx.execute(
            "DELETE FROM entry_embeddings WHERE journal_id = ?1 AND entry_id = ?2",
            params![journal_id, entry_id],
        )?;
        tx.execute(
            "DELETE FROM entry_embeddings_vec WHERE journal_id = ?1 AND entry_id = ?2",
            params![journal_id, entry_id],
        )?;
        tx.execute(
            "DELETE FROM unread_entries WHERE journal_id = ?1 AND entry_id = ?2",
            params![journal_id, entry_id],
        )?;
        tx.execute(
            "DELETE FROM entries WHERE journal_id = ?1 AND id = ?2",
            params![journal_id, entry_id],
        )?;
        tx.execute(
            "DELETE FROM sync_cursors WHERE resource_key = ?1",
            params![format!("comments:{journal_id}:{entry_id}")],
        )?;
        tx.execute(
            r#"
            DELETE FROM sync_outbox
            WHERE status != 'failed' AND last_error IS NULL AND (
              (resource = 'entry' AND object_id = ?1)
              OR (resource IN ('original_media', 'comment') AND instr(object_id, ?1 || ':') = 1)
            )
            "#,
            params![format!("{journal_id}:{entry_id}")],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_json_rows(&self, table: &str) -> StoreResult<Vec<String>> {
        let conn = self.connect()?;
        let sql = format!("SELECT data_json FROM {table}");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut data = Vec::new();
        for row in rows {
            data.push(row?);
        }
        Ok(data)
    }

    pub fn list_json_rows_paged(
        &self,
        table: ListJsonTable,
        page: &ListPageRequest,
    ) -> StoreResult<ListPageResult> {
        let conn = self.connect()?;
        let (offset, limit_plus_one) = sqlite_page_params(page)?;
        let cursor_key = decode_list_cursor(page.cursor.as_deref())?;
        let cursor_updated = cursor_key.as_ref().map(|key| key.updated_at.as_str());
        let cursor_id = cursor_key.as_ref().map(|key| key.id.as_str());
        let table_name = table.as_sql_identifier();
        let sql = format!(
            r#"
            SELECT data_json, COALESCE(updated_at, '') AS updated_at_key, id
            FROM {table_name}
            WHERE (
                :cursor_updated IS NULL
                OR COALESCE(updated_at, '') < :cursor_updated
                OR (COALESCE(updated_at, '') = :cursor_updated AND id < :cursor_id)
            )
            ORDER BY COALESCE(updated_at, '') DESC, id DESC
            LIMIT :limit_value OFFSET :offset_value
            "#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! {
                ":cursor_updated": cursor_updated,
                ":cursor_id": cursor_id,
                ":limit_value": limit_plus_one,
                ":offset_value": offset,
            },
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        build_page_result(items, page.limit)
    }

    pub fn list_journals_json_rows_paged(
        &self,
        page: &ListPageRequest,
        include_deleted: bool,
    ) -> StoreResult<ListPageResult> {
        let conn = self.connect()?;
        let (offset, limit_plus_one) = sqlite_page_params(page)?;
        let cursor_key = decode_list_cursor(page.cursor.as_deref())?;
        let cursor_updated = cursor_key.as_ref().map(|key| key.updated_at.as_str());
        let cursor_id = cursor_key.as_ref().map(|key| key.id.as_str());
        let sql = r#"
            SELECT data_json, COALESCE(updated_at, '') AS updated_at_key, id
            FROM journals
            WHERE (
                :include_deleted = 1
                OR (
                    is_deleted = 0
                    AND deleted_at IS NULL
                    AND COALESCE(json_extract(data_json, '$.state'), '') != 'deleted'
                )
            )
              AND (
                :cursor_updated IS NULL
                OR COALESCE(updated_at, '') < :cursor_updated
                OR (COALESCE(updated_at, '') = :cursor_updated AND id < :cursor_id)
              )
            ORDER BY COALESCE(updated_at, '') DESC, id DESC
            LIMIT :limit_value OFFSET :offset_value
            "#;
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(
            named_params! {
                ":include_deleted": if include_deleted { 1 } else { 0 },
                ":cursor_updated": cursor_updated,
                ":cursor_id": cursor_id,
                ":limit_value": limit_plus_one,
                ":offset_value": offset,
            },
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        build_page_result(items, page.limit)
    }
}
