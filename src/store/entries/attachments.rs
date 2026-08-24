use rusqlite::{OptionalExtension, params};

use crate::store::StoreResult;
use crate::store::sqlite::{EntryAttachmentRow, EntryAttachmentUpsert, Store};

impl Store {
    pub fn upsert_entry_attachment(&self, attachment: &EntryAttachmentUpsert) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO entry_attachments (
              journal_id, entry_id, moment_id, file_path, moment_type, content_type, file_size_bytes, md5_body,
              width, height, duration_seconds, thumbnail_content_type, thumbnail_md5, thumbnail_width,
              thumbnail_height, thumbnail_file_size_bytes, thumbnail_bytes,
              entry_associated, original_uploaded, last_error, created_at, updated_at
            )
            VALUES (
              ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
              ?9, ?10, ?11, ?12, ?13, ?14,
              ?15, ?16, ?17,
              0, 0, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
            )
            ON CONFLICT(journal_id, entry_id, moment_id) DO UPDATE SET
              file_path = excluded.file_path,
              moment_type = excluded.moment_type,
              content_type = excluded.content_type,
              file_size_bytes = excluded.file_size_bytes,
              md5_body = excluded.md5_body,
              width = excluded.width,
              height = excluded.height,
              duration_seconds = excluded.duration_seconds,
              thumbnail_content_type = excluded.thumbnail_content_type,
              thumbnail_md5 = excluded.thumbnail_md5,
              thumbnail_width = excluded.thumbnail_width,
              thumbnail_height = excluded.thumbnail_height,
              thumbnail_file_size_bytes = excluded.thumbnail_file_size_bytes,
              thumbnail_bytes = excluded.thumbnail_bytes,
              last_error = NULL,
              updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                attachment.journal_id,
                attachment.entry_id,
                attachment.moment_id,
                attachment.file_path,
                attachment.moment_type,
                attachment.content_type,
                attachment.file_size_bytes,
                attachment.md5_body,
                attachment.width,
                attachment.height,
                attachment.duration_seconds,
                attachment.thumbnail_content_type,
                attachment.thumbnail_md5,
                attachment.thumbnail_width,
                attachment.thumbnail_height,
                attachment.thumbnail_file_size_bytes,
                attachment.thumbnail_bytes
            ],
        )?;
        Ok(())
    }

    pub fn list_entry_attachments(
        &self,
        journal_id: &str,
        entry_id: &str,
    ) -> StoreResult<Vec<EntryAttachmentRow>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT
              journal_id, entry_id, moment_id, file_path, moment_type, content_type, file_size_bytes, md5_body,
              width, height, duration_seconds, thumbnail_content_type, thumbnail_md5, thumbnail_width,
              thumbnail_height, thumbnail_file_size_bytes, thumbnail_bytes,
              entry_associated, original_uploaded, last_error
            FROM entry_attachments
            WHERE journal_id = ?1 AND entry_id = ?2
            ORDER BY created_at ASC, moment_id ASC
            "#,
        )?;
        let rows = stmt.query_map(params![journal_id, entry_id], |row| {
            Ok(EntryAttachmentRow {
                journal_id: row.get(0)?,
                entry_id: row.get(1)?,
                moment_id: row.get(2)?,
                file_path: row.get(3)?,
                moment_type: row.get(4)?,
                content_type: row.get(5)?,
                file_size_bytes: row.get(6)?,
                md5_body: row.get(7)?,
                width: row.get(8)?,
                height: row.get(9)?,
                duration_seconds: row.get(10)?,
                thumbnail_content_type: row.get(11)?,
                thumbnail_md5: row.get(12)?,
                thumbnail_width: row.get(13)?,
                thumbnail_height: row.get(14)?,
                thumbnail_file_size_bytes: row.get(15)?,
                thumbnail_bytes: row.get(16)?,
                entry_associated: row.get::<_, i64>(17)? != 0,
                original_uploaded: row.get::<_, i64>(18)? != 0,
                last_error: row.get(19)?,
            })
        })?;
        let mut attachments = Vec::new();
        for row in rows {
            attachments.push(row?);
        }
        Ok(attachments)
    }

    pub fn get_entry_attachment(
        &self,
        journal_id: &str,
        entry_id: &str,
        moment_id: &str,
    ) -> StoreResult<Option<EntryAttachmentRow>> {
        let conn = self.connect()?;
        conn.query_row(
            r#"
            SELECT
              journal_id, entry_id, moment_id, file_path, moment_type, content_type, file_size_bytes, md5_body,
              width, height, duration_seconds, thumbnail_content_type, thumbnail_md5, thumbnail_width,
              thumbnail_height, thumbnail_file_size_bytes, thumbnail_bytes,
              entry_associated, original_uploaded, last_error
            FROM entry_attachments
            WHERE journal_id = ?1 AND entry_id = ?2 AND moment_id = ?3
            LIMIT 1
            "#,
            params![journal_id, entry_id, moment_id],
            |row| {
                Ok(EntryAttachmentRow {
                    journal_id: row.get(0)?,
                    entry_id: row.get(1)?,
                    moment_id: row.get(2)?,
                    file_path: row.get(3)?,
                    moment_type: row.get(4)?,
                    content_type: row.get(5)?,
                    file_size_bytes: row.get(6)?,
                    md5_body: row.get(7)?,
                    width: row.get(8)?,
                    height: row.get(9)?,
                    duration_seconds: row.get(10)?,
                    thumbnail_content_type: row.get(11)?,
                    thumbnail_md5: row.get(12)?,
                    thumbnail_width: row.get(13)?,
                    thumbnail_height: row.get(14)?,
                    thumbnail_file_size_bytes: row.get(15)?,
                    thumbnail_bytes: row.get(16)?,
                    entry_associated: row.get::<_, i64>(17)? != 0,
                    original_uploaded: row.get::<_, i64>(18)? != 0,
                    last_error: row.get(19)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn mark_entry_attachments_associated(
        &self,
        journal_id: &str,
        entry_id: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            UPDATE entry_attachments
            SET entry_associated = 1,
                last_error = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE journal_id = ?1 AND entry_id = ?2
            "#,
            params![journal_id, entry_id],
        )?;
        Ok(())
    }

    pub fn mark_entry_attachment_original_uploaded(
        &self,
        journal_id: &str,
        entry_id: &str,
        moment_id: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            UPDATE entry_attachments
            SET original_uploaded = 1,
                last_error = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE journal_id = ?1 AND entry_id = ?2 AND moment_id = ?3
            "#,
            params![journal_id, entry_id, moment_id],
        )?;
        Ok(())
    }

    pub fn mark_entry_attachment_error(
        &self,
        journal_id: &str,
        entry_id: &str,
        moment_id: &str,
        error: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            UPDATE entry_attachments
            SET last_error = ?4,
                updated_at = CURRENT_TIMESTAMP
            WHERE journal_id = ?1 AND entry_id = ?2 AND moment_id = ?3
            "#,
            params![journal_id, entry_id, moment_id, error],
        )?;
        Ok(())
    }
}
