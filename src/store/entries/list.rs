use rusqlite::{OptionalExtension, named_params, params};

use crate::store::json_rows::{build_page_result, decode_list_cursor, sqlite_page_params};
use crate::store::sqlite::{
    EntryListSort, EntryListSortMethod, ListPageRequest, ListPageResult, Store,
};
use crate::store::{StoreError, StoreResult};

impl Store {
    pub fn get_entry_user_edit_date(&self, id: &str) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT user_edit_date FROM entries WHERE id = ?1 LIMIT 1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|v| v.flatten())
        .map_err(Into::into)
    }

    pub fn get_entry_journal_id(&self, id: &str) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT journal_id FROM entries WHERE id = ?1 LIMIT 1",
            params![id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(dead_code)]
    pub fn list_entries_json_rows_by_journal_id(
        &self,
        journal_id: &str,
        limit: Option<usize>,
        sort: EntryListSort,
        sort_method: EntryListSortMethod,
    ) -> StoreResult<Vec<String>> {
        let page = ListPageRequest {
            limit,
            offset: None,
            cursor: None,
        };
        Ok(self
            .list_entries_json_rows_by_journal_id_paged(journal_id, &page, sort, sort_method)?
            .rows)
    }

    pub fn list_entries_json_rows_by_journal_id_paged(
        &self,
        journal_id: &str,
        page: &ListPageRequest,
        sort: EntryListSort,
        sort_method: EntryListSortMethod,
    ) -> StoreResult<ListPageResult> {
        let conn = self.connect()?;
        let (offset, limit_plus_one) = sqlite_page_params(page)?;
        // The `date` column is always NULL or numeric REAL (populated by
        // extract_entry_fields / migration 0007), so no ISO-string branch is needed.
        let sort_key_expr = match sort_method {
            EntryListSortMethod::EntryDate => "COALESCE(CAST(date AS INTEGER), 0)",
            EntryListSortMethod::EditDate => {
                "CASE
                    WHEN user_edit_date IS NULL OR user_edit_date = '' THEN 0
                    WHEN user_edit_date GLOB '*[^0-9]*' THEN COALESCE(
                        CAST((julianday(user_edit_date) - 2440587.5) * 86400000 AS INTEGER),
                        0
                    )
                    ELSE CAST(user_edit_date AS INTEGER)
                 END"
            }
        };
        let sort_clause = match sort {
            EntryListSort::Asc => "ASC",
            EntryListSort::Desc => "DESC",
        };
        let cursor_op = match sort {
            EntryListSort::Asc => ">",
            EntryListSort::Desc => "<",
        };
        let cursor_key = decode_list_cursor(page.cursor.as_deref())?;
        let cursor_sort_key = cursor_key
            .as_ref()
            .map(|key| {
                key.updated_at.trim().parse::<i64>().map_err(|_| {
                    StoreError::invalid_input("cursor is invalid: expected numeric sort key")
                })
            })
            .transpose()?;
        let cursor_id = cursor_key.as_ref().map(|key| key.id.as_str());
        // Note: we return raw `data_json` rather than calling `rebuild_entry_content`.
        // Normalization happens at write time in `upsert_entry_json_row`, so data_json
        // is already in the canonical shape for all entries written through Rust code.
        let sql = format!(
            r#"
            WITH entries_with_sort AS (
              SELECT
                data_json,
                id,
                ({sort_key_expr}) AS sort_key
              FROM entries
              WHERE journal_id = :journal_id AND is_deleted = 0
            )
            SELECT
              data_json,
              CAST(sort_key AS TEXT) AS updated_at_key,
              id
            FROM entries_with_sort
            WHERE (
              :cursor_sort_key IS NULL
              OR sort_key {cursor_op} :cursor_sort_key
              OR (
                sort_key = :cursor_sort_key
                AND id {cursor_op} :cursor_id
              )
            )
            ORDER BY sort_key {sort_clause}, id {sort_clause}
            LIMIT :limit_value OFFSET :offset_value
            "#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! {
                ":journal_id": journal_id,
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
        build_page_result(items, page.limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::Store;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_at(dir.path().join("dayone.db")).expect("store");
        (dir, store)
    }

    #[test]
    fn list_hides_deleted_entries() {
        let (_dir, store) = store();
        store
            .upsert_entry_json_row(
                "live",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"live","date":1000}"#,
            )
            .expect("live entry");
        // A tombstoned entry: passing a deleted_at edit date flips is_deleted=1.
        store
            .upsert_entry_json_row_with_edit_date(
                "gone",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                Some("2026-03-18T00:00:00.000Z"),
                Some("2026-03-18T00:00:00.000Z"),
                r#"{"id":"gone","date":2000,"deleted_at":"2026-03-18T00:00:00.000Z"}"#,
            )
            .expect("deleted entry");

        let page = ListPageRequest {
            limit: None,
            offset: None,
            cursor: None,
        };
        let result = store
            .list_entries_json_rows_by_journal_id_paged(
                "journal-1",
                &page,
                EntryListSort::Desc,
                EntryListSortMethod::EntryDate,
            )
            .expect("list should work");
        assert_eq!(result.rows.len(), 1, "deleted entry should be excluded");
        assert!(result.rows[0].contains("\"live\""));
    }
}
