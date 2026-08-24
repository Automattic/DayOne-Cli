use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rusqlite::{OptionalExtension, named_params, params};
use serde::Deserialize;
use serde_json::Value;

use crate::comment_flags::comment_deleted_flag;
use crate::store::json_rows::sqlite_page_params;
use crate::store::sqlite::{ListPageRequest, ListPageResult, Store};
use crate::store::{StoreError, StoreResult};

const SQLITE_MAX_VARIABLE_NUMBER: usize = 999;
const COMMENT_REACTION_QUERY_FIXED_PARAMS: usize = 2;

fn comment_string_field(value: &Value, snake: &str, camel: &str) -> Option<String> {
    value
        .get(snake)
        .or_else(|| value.get(camel))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

#[derive(Debug, Deserialize)]
struct CommentCursorPayload {
    created_at: String,
    id: String,
}

fn encode_comment_cursor(created_at: &str, id: &str) -> String {
    let payload = serde_json::json!({
        "created_at": created_at,
        "id": id,
    });
    BASE64_STANDARD.encode(payload.to_string())
}

fn decode_comment_cursor(cursor: Option<&str>) -> StoreResult<Option<(String, String)>> {
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
    let payload: CommentCursorPayload = serde_json::from_slice(&decoded)
        .map_err(|_| StoreError::invalid_input("cursor is invalid: expected encoded JSON token"))?;
    if payload.id.trim().is_empty() {
        return Err(StoreError::invalid_input("cursor is invalid: missing id"));
    }
    let sort_key = payload.created_at.trim();
    if sort_key.is_empty() {
        return Err(StoreError::invalid_input(
            "cursor is invalid: missing created_at",
        ));
    }
    Ok(Some((sort_key.to_owned(), payload.id)))
}

impl Store {
    pub fn comments_cursor_key(journal_id: &str, entry_id: &str) -> String {
        format!("comments:{journal_id}:{entry_id}")
    }

    pub fn upsert_comment_json_row(&self, data_json: &str) -> StoreResult<()> {
        let value: Value = serde_json::from_str(data_json)
            .map_err(|_| StoreError::invalid_input("invalid JSON in comment data_json"))?;
        let id = non_empty(comment_string_field(&value, "id", "id"))
            .ok_or_else(|| StoreError::invalid_input("comment data_json missing or empty id"))?;
        let journal_id = non_empty(comment_string_field(&value, "journal_id", "journalId"))
            .ok_or_else(|| {
                StoreError::invalid_input("comment data_json missing or empty journal_id")
            })?;
        let entry_id =
            non_empty(comment_string_field(&value, "entry_id", "entryId")).ok_or_else(|| {
                StoreError::invalid_input("comment data_json missing or empty entry_id")
            })?;
        let author_id = non_empty(comment_string_field(&value, "author_id", "authorId"));
        let created_at = non_empty(comment_string_field(&value, "created_at", "createdAt"));
        let updated_at = non_empty(comment_string_field(&value, "updated_at", "updatedAt"));
        let deleted_at = non_empty(comment_string_field(&value, "deleted_at", "deletedAt"));
        let is_deleted = if comment_deleted_flag(&value) { 1 } else { 0 };
        let content = non_empty(comment_string_field(&value, "content", "content"));
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO comments (
              id, journal_id, entry_id, author_id, created_at, updated_at, deleted_at, is_deleted, content, data_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
              journal_id = excluded.journal_id,
              entry_id = excluded.entry_id,
              author_id = excluded.author_id,
              created_at = excluded.created_at,
              updated_at = excluded.updated_at,
              deleted_at = excluded.deleted_at,
              is_deleted = excluded.is_deleted,
              content = excluded.content,
              data_json = excluded.data_json
            "#,
            params![
                id,
                journal_id,
                entry_id,
                author_id,
                created_at,
                updated_at,
                deleted_at,
                is_deleted,
                content,
                data_json
            ],
        )?;
        Ok(())
    }

    pub fn get_comment_json_row_by_id(&self, comment_id: &str) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT data_json FROM comments WHERE id = ?1 LIMIT 1",
            params![comment_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(dead_code)]
    pub fn delete_comment_json_row_by_id(&self, comment_id: &str) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM comments WHERE id = ?1", params![comment_id])?;
        Ok(())
    }

    pub fn list_comments_json_rows_by_entry_paged(
        &self,
        journal_id: &str,
        entry_id: &str,
        page: &ListPageRequest,
    ) -> StoreResult<ListPageResult> {
        let conn = self.connect()?;
        let (offset, limit_plus_one) = sqlite_page_params(page)?;
        let cursor_key = decode_comment_cursor(page.cursor.as_deref())?;
        let cursor_sort_key = cursor_key.as_ref().map(|key| key.0.as_str());
        let cursor_id = cursor_key.as_ref().map(|key| key.1.as_str());
        let mut stmt = conn.prepare(
            r#"
            SELECT data_json, created_at AS created_at_key, id
            FROM comments
            WHERE journal_id = :journal_id
              AND entry_id = :entry_id
              AND is_deleted = 0
              AND created_at IS NOT NULL
              AND created_at <> ''
              AND (
                :cursor_sort_key IS NULL
                OR created_at > :cursor_sort_key
                OR (created_at = :cursor_sort_key AND id > :cursor_id)
              )
            ORDER BY created_at ASC, id ASC
            LIMIT :limit_value OFFSET :offset_value
            "#,
        )?;
        let rows = stmt.query_map(
            named_params! {
                ":journal_id": journal_id,
                ":entry_id": entry_id,
                ":cursor_sort_key": cursor_sort_key,
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
        let mut has_more = false;
        if let Some(limit) = page.limit
            && items.len() > limit
        {
            has_more = true;
            items.truncate(limit);
        }
        let next_cursor = if has_more {
            items
                .last()
                .map(|(_, created_at_key, id)| encode_comment_cursor(created_at_key, id))
        } else {
            None
        };
        Ok(ListPageResult {
            rows: items.into_iter().map(|(row, _, _)| row).collect(),
            has_more,
            next_cursor,
        })
    }

    pub fn upsert_comment_reaction_json_row(&self, data_json: &str) -> StoreResult<()> {
        let value: Value = serde_json::from_str(data_json)
            .map_err(|_| StoreError::invalid_input("invalid JSON in comment reaction data_json"))?;
        let id = non_empty(comment_string_field(&value, "id", "id")).ok_or_else(|| {
            StoreError::invalid_input("comment reaction data_json missing or empty id")
        })?;
        let journal_id = non_empty(comment_string_field(&value, "journal_id", "journalId"))
            .ok_or_else(|| {
                StoreError::invalid_input("comment reaction data_json missing or empty journal_id")
            })?;
        let entry_id =
            non_empty(comment_string_field(&value, "entry_id", "entryId")).ok_or_else(|| {
                StoreError::invalid_input("comment reaction data_json missing or empty entry_id")
            })?;
        let comment_id = non_empty(comment_string_field(&value, "comment_id", "commentId"))
            .ok_or_else(|| {
                StoreError::invalid_input("comment reaction data_json missing or empty comment_id")
            })?;
        let user_id =
            non_empty(comment_string_field(&value, "user_id", "userId")).ok_or_else(|| {
                StoreError::invalid_input("comment reaction data_json missing or empty user_id")
            })?;
        let reaction =
            non_empty(comment_string_field(&value, "reaction", "reaction")).ok_or_else(|| {
                StoreError::invalid_input("comment reaction data_json missing or empty reaction")
            })?;
        let timestamp = non_empty(comment_string_field(&value, "timestamp", "timestamp"));
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        // Ensure the identity key stays unique even when server IDs are rotated.
        tx.execute(
            r#"
            DELETE FROM comment_reactions
            WHERE journal_id = ?1
              AND entry_id = ?2
              AND comment_id = ?3
              AND user_id = ?4
              AND id != ?5
            "#,
            params![journal_id, entry_id, comment_id, user_id, id],
        )?;
        tx.execute(
            r#"
            INSERT INTO comment_reactions (
              id, journal_id, entry_id, comment_id, user_id, reaction, timestamp, data_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
              journal_id = excluded.journal_id,
              entry_id = excluded.entry_id,
              comment_id = excluded.comment_id,
              user_id = excluded.user_id,
              reaction = excluded.reaction,
              timestamp = excluded.timestamp,
              data_json = excluded.data_json
            "#,
            params![
                id, journal_id, entry_id, comment_id, user_id, reaction, timestamp, data_json
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn list_comment_reactions_json_rows_by_comment(
        &self,
        journal_id: &str,
        entry_id: &str,
        comment_id: &str,
    ) -> StoreResult<Vec<String>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT data_json
            FROM comment_reactions
            WHERE journal_id = ?1 AND entry_id = ?2 AND comment_id = ?3
            ORDER BY COALESCE(timestamp, '') ASC, id ASC
            "#,
        )?;
        let rows = stmt.query_map(params![journal_id, entry_id, comment_id], |row| {
            row.get::<_, String>(0)
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    #[allow(dead_code)]
    pub fn list_comment_reactions_json_rows_by_entry(
        &self,
        journal_id: &str,
        entry_id: &str,
    ) -> StoreResult<Vec<(String, String)>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT comment_id, data_json
            FROM comment_reactions
            WHERE journal_id = ?1 AND entry_id = ?2
            ORDER BY COALESCE(timestamp, '') ASC, id ASC
            "#,
        )?;
        let rows = stmt.query_map(params![journal_id, entry_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn list_comment_reactions_json_rows_by_comment_ids(
        &self,
        journal_id: &str,
        entry_id: &str,
        comment_ids: &[String],
    ) -> StoreResult<Vec<(String, String)>> {
        if comment_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.connect()?;
        let mut items = Vec::new();
        let chunk_size = SQLITE_MAX_VARIABLE_NUMBER - COMMENT_REACTION_QUERY_FIXED_PARAMS;
        for ids_chunk in comment_ids.chunks(chunk_size) {
            let placeholders = std::iter::repeat_n("?", ids_chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                r#"
                SELECT comment_id, data_json
                FROM comment_reactions
                WHERE journal_id = ?1
                  AND entry_id = ?2
                  AND comment_id IN ({placeholders})
                ORDER BY COALESCE(timestamp, '') ASC, id ASC
                "#
            );
            let mut params: Vec<&dyn rusqlite::ToSql> =
                Vec::with_capacity(COMMENT_REACTION_QUERY_FIXED_PARAMS + ids_chunk.len());
            params.push(&journal_id);
            params.push(&entry_id);
            for id in ids_chunk {
                params.push(id);
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                items.push(row?);
            }
        }
        Ok(items)
    }

    pub fn delete_comment_reaction_by_user(
        &self,
        journal_id: &str,
        entry_id: &str,
        comment_id: &str,
        user_id: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            DELETE FROM comment_reactions
            WHERE journal_id = ?1 AND entry_id = ?2 AND comment_id = ?3 AND user_id = ?4
            "#,
            params![journal_id, entry_id, comment_id, user_id],
        )?;
        Ok(())
    }

    pub fn delete_comment_reactions_by_comment(
        &self,
        journal_id: &str,
        entry_id: &str,
        comment_id: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            DELETE FROM comment_reactions
            WHERE journal_id = ?1 AND entry_id = ?2 AND comment_id = ?3
            "#,
            params![journal_id, entry_id, comment_id],
        )?;
        Ok(())
    }

    pub fn remap_comment_local_id(
        &self,
        journal_id: &str,
        entry_id: &str,
        from_comment_id: &str,
        to_comment_id: &str,
    ) -> StoreResult<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM comments WHERE id = ?1 AND journal_id = ?2 AND entry_id = ?3)",
            params![to_comment_id, journal_id, entry_id],
            |row| row.get(0),
        )?;
        if target_exists {
            tx.execute(
                "DELETE FROM comments WHERE id = ?1 AND journal_id = ?2 AND entry_id = ?3",
                params![from_comment_id, journal_id, entry_id],
            )?;
            tx.execute(
                "DELETE FROM comment_reactions WHERE comment_id = ?1 AND journal_id = ?2 AND entry_id = ?3",
                params![from_comment_id, journal_id, entry_id],
            )?;
        } else {
            tx.execute(
                "UPDATE comments SET id = ?4, data_json = json_set(data_json, '$.id', ?4) WHERE id = ?1 AND journal_id = ?2 AND entry_id = ?3",
                params![from_comment_id, journal_id, entry_id, to_comment_id],
            )?;
            tx.execute(
                "UPDATE comment_reactions SET comment_id = ?4, data_json = json_set(json_set(data_json, '$.comment_id', ?4), '$.commentId', ?4) WHERE comment_id = ?1 AND journal_id = ?2 AND entry_id = ?3",
                params![from_comment_id, journal_id, entry_id, to_comment_id],
            )?;
        }

        let from_object = format!("{journal_id}:{entry_id}:{from_comment_id}");
        let to_object = format!("{journal_id}:{entry_id}:{to_comment_id}");
        tx.execute(
            r#"
            UPDATE sync_outbox
            SET
              object_id = ?2,
              payload_json = json_set(payload_json, '$.id', ?3),
              updated_at = CURRENT_TIMESTAMP
            WHERE resource = 'comment'
              AND object_id = ?1
              AND COALESCE(status, '') <> 'processing'
            "#,
            params![from_object, to_object, to_comment_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
