use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::store::sqlite::{
    EntryListSort, EntryListSortMethod, ListJsonTable, ListPageRequest, ListPageResult, Store,
};
use crate::util::normalize_yyyy_mm_dd;

#[derive(Debug, Clone, Default)]
pub struct PaginationInput {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListOutput {
    pub ok: bool,
    pub count: usize,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub items: Vec<Value>,
}

#[derive(Debug, Serialize)]
pub struct ListMessagesOutput {
    pub ok: bool,
    pub daily_chat_id: String,
    pub count: usize,
    pub messages: Vec<Value>,
}

pub fn journals(
    store: &Store,
    pagination: PaginationInput,
    include_deleted: bool,
) -> Result<ListOutput> {
    let rows =
        store.list_journals_json_rows_paged(&to_page_request(&pagination), include_deleted)?;
    build_list_output(rows, &pagination, None)
}

pub fn entries_by_journal_id(
    store: &Store,
    journal_id: &str,
    pagination: PaginationInput,
    sort: EntryListSort,
    sort_method: EntryListSortMethod,
    fields: Option<&[String]>,
) -> Result<ListOutput> {
    let rows = store.list_entries_json_rows_by_journal_id_paged(
        journal_id,
        &to_page_request(&pagination),
        sort,
        sort_method,
    )?;
    build_list_output(rows, &pagination, fields)
}

pub fn daily_chat(store: &Store, pagination: PaginationInput) -> Result<ListOutput> {
    let rows =
        store.list_json_rows_paged(ListJsonTable::DailyChatFeed, &to_page_request(&pagination))?;
    build_list_output(rows, &pagination, None)
}

pub fn daily_chat_messages_by_id(store: &Store, daily_chat_id: &str) -> Result<ListMessagesOutput> {
    let normalized_date = normalize_yyyy_mm_dd(daily_chat_id);
    let trimmed = daily_chat_id.trim();
    let mut resolved_daily_chat_id = daily_chat_id.to_owned();
    let mut row = store.get_json_row_by_id("daily_chat_feed", daily_chat_id)?;
    if row.is_none() && trimmed != daily_chat_id {
        row = store.get_json_row_by_id("daily_chat_feed", trimmed)?;
        if row.is_some() {
            resolved_daily_chat_id = trimmed.to_owned();
        }
    }
    if row.is_none()
        && let Some(date) = normalized_date
        && let Some((resolved_id, resolved_row)) = store.get_daily_chat_row_by_date(&date)?
    {
        resolved_daily_chat_id = resolved_id;
        row = Some(resolved_row);
    }
    let mut messages: Vec<Value> = Vec::new();
    if let Some(row) = row {
        let value: Value = serde_json::from_str(&row).with_context(|| {
            format!("failed to parse daily chat row for id {resolved_daily_chat_id}")
        })?;
        if let Some(array) = value.get("chat_history").and_then(Value::as_array) {
            messages = array.clone();
        } else if let Some(array) = value.get("messages").and_then(Value::as_array) {
            messages = array.clone();
        } else if let Some(array) = value
            .get("data")
            .and_then(|data| data.get("messages"))
            .and_then(Value::as_array)
        {
            messages = array.clone();
        }
    }

    Ok(ListMessagesOutput {
        ok: true,
        daily_chat_id: resolved_daily_chat_id,
        count: messages.len(),
        messages,
    })
}

pub fn context_items(store: &Store, pagination: PaginationInput) -> Result<ListOutput> {
    let rows = store.list_json_rows_paged(
        ListJsonTable::ContextLibraryItems,
        &to_page_request(&pagination),
    )?;
    build_list_output(rows, &pagination, None)
}

fn to_page_request(pagination: &PaginationInput) -> ListPageRequest {
    ListPageRequest {
        limit: pagination.limit,
        offset: pagination.offset,
        cursor: pagination.cursor.clone(),
    }
}

fn build_list_output(
    page: ListPageResult,
    pagination: &PaginationInput,
    fields: Option<&[String]>,
) -> Result<ListOutput> {
    let items = parse_rows_with_projection(page.rows, fields)?;
    Ok(ListOutput {
        ok: true,
        count: items.len(),
        limit: pagination.limit,
        offset: pagination.offset,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
        items,
    })
}

fn parse_rows(rows: Vec<String>) -> Result<Vec<Value>> {
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let value: Value =
            serde_json::from_str(&row).context("failed to decode stored JSON row")?;
        items.push(value);
    }
    Ok(items)
}

fn parse_rows_with_projection(rows: Vec<String>, fields: Option<&[String]>) -> Result<Vec<Value>> {
    match fields {
        Some(fields) => project_rows(rows, fields),
        None => parse_rows(rows),
    }
}

fn project_rows(rows: Vec<String>, fields: &[String]) -> Result<Vec<Value>> {
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let value: Value =
            serde_json::from_str(&row).context("failed to decode stored JSON row")?;
        let object = value
            .as_object()
            .context("expected stored JSON row to be an object")?;
        let projected = project_entry_object(object, fields);
        items.push(Value::Object(projected));
    }
    Ok(items)
}

fn project_entry_object(object: &Map<String, Value>, fields: &[String]) -> Map<String, Value> {
    let mut projected = Map::with_capacity(fields.len());
    for field in fields {
        // `body` is intentionally projected exactly as stored (no truncation).
        let projected_value = object.get(field).cloned().unwrap_or(Value::Null);
        projected.insert(field.clone(), projected_value);
    }
    projected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_store_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("dayone-cli-list-{name}-{unique}.db"))
    }

    #[test]
    fn resolves_daily_chat_messages_when_date_is_passed_instead_of_id() {
        let path = test_store_path("date-fallback");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_json_row(
                "daily_chat_feed",
                "chat-abc-123",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{
                    "id":"chat-abc-123",
                    "date":"2026-03-17",
                    "chat_history":[{"id":"m1","role":"user","content":"Hello!"}]
                }"#,
            )
            .expect("daily chat row should save");

        let output =
            daily_chat_messages_by_id(&store, "2026-03-17").expect("list by date should work");
        assert_eq!(output.daily_chat_id, "chat-abc-123");
        assert_eq!(output.count, 1);
        assert_eq!(output.messages[0]["content"], "Hello!");
    }

    #[test]
    fn date_shape_validation_works() {
        assert!(normalize_yyyy_mm_dd("2026-03-17").is_some());
        assert!(normalize_yyyy_mm_dd(" 2026-03-17 ").is_some());
        assert!(normalize_yyyy_mm_dd("2026/03/17").is_none());
        assert!(normalize_yyyy_mm_dd("chat-id-123").is_none());
    }

    #[test]
    fn resolves_daily_chat_messages_with_whitespace_date_input() {
        let path = test_store_path("date-fallback-whitespace");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_json_row(
                "daily_chat_feed",
                "chat-whitespace-123",
                Some("2026-03-17T13:00:00.000Z"),
                None,
                r#"{
                    "id":"chat-whitespace-123",
                    "date":"2026-03-17",
                    "chat_history":[{"id":"m1","role":"user","content":"Hello!"}]
                }"#,
            )
            .expect("daily chat row should save");

        let output = daily_chat_messages_by_id(&store, " 2026-03-17 ").expect("list should work");
        assert_eq!(output.daily_chat_id, "chat-whitespace-123");
        assert_eq!(output.count, 1);
    }

    #[test]
    fn entries_projection_returns_only_requested_fields() {
        let path = test_store_path("entries-projection");
        let store = Store::open_at(&path).expect("store should open");
        let journal_id = "j-projection";
        store
            .upsert_entry_json_row(
                "entry-proj-1",
                journal_id,
                Some("2026-03-17T10:00:00.000Z"),
                None,
                r#"{
                    "id":"entry-proj-1",
                    "date":"2026-03-17T10:00:00.000Z",
                    "tags":["ship","ops"],
                    "body":"USS Voyager\nCaptain's log",
                    "weather":{"conditionCode":"clear"},
                    "richTextJSON":{"ops":[{"insert":"hello"}]}
                }"#,
            )
            .expect("entry should save");

        let fields = vec!["date".to_owned(), "tags".to_owned()];
        let output = entries_by_journal_id(
            &store,
            journal_id,
            PaginationInput::default(),
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
            Some(&fields),
        )
        .expect("projected list should work");

        assert_eq!(output.count, 1);
        assert!(output.items[0].get("date").is_some());
        assert!(output.items[0].get("tags").is_some());
        assert!(output.items[0].get("body").is_none());
        assert!(output.items[0].get("weather").is_none());
    }

    #[test]
    fn entries_projection_keeps_full_body_value() {
        let path = test_store_path("entries-projection-body");
        let store = Store::open_at(&path).expect("store should open");
        let journal_id = "j-projection-body";
        let body = "USS Enterprise\nStardate 41153.7\nSecond line";
        store
            .upsert_entry_json_row(
                "entry-proj-body",
                journal_id,
                Some("2026-03-17T11:00:00.000Z"),
                None,
                &format!(
                    r#"{{"id":"entry-proj-body","date":"2026-03-17T11:00:00.000Z","body":{}}}"#,
                    serde_json::to_string(body).expect("body should serialize")
                ),
            )
            .expect("entry should save");

        let fields = vec!["body".to_owned()];
        let output = entries_by_journal_id(
            &store,
            journal_id,
            PaginationInput::default(),
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
            Some(&fields),
        )
        .expect("projected list should work");

        assert_eq!(output.count, 1);
        assert_eq!(output.items[0]["body"], Value::String(body.to_owned()));
    }

    #[test]
    fn entries_projection_sets_null_for_unknown_fields() {
        let path = test_store_path("entries-projection-unknown-field");
        let store = Store::open_at(&path).expect("store should open");
        let journal_id = "j-projection-unknown";
        store
            .upsert_entry_json_row(
                "entry-proj-unknown",
                journal_id,
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{"id":"entry-proj-unknown","date":"2026-03-17T12:00:00.000Z"}"#,
            )
            .expect("entry should save");

        let fields = vec!["date".to_owned(), "shipName".to_owned()];
        let output = entries_by_journal_id(
            &store,
            journal_id,
            PaginationInput::default(),
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
            Some(&fields),
        )
        .expect("projected list should work");

        assert_eq!(
            output.items[0]["date"],
            Value::String("2026-03-17T12:00:00.000Z".to_owned())
        );
        assert_eq!(output.items[0]["shipName"], Value::Null);
    }

    #[test]
    fn entries_output_includes_pagination_metadata() {
        let path = test_store_path("entries-pagination-metadata");
        let store = Store::open_at(&path).expect("store should open");
        let journal_id = "j-pagination";
        for (id, updated_at) in [
            ("entry-1", "2026-03-01T00:00:00.000Z"),
            ("entry-2", "2026-03-02T00:00:00.000Z"),
            ("entry-3", "2026-03-03T00:00:00.000Z"),
        ] {
            store
                .upsert_entry_json_row(
                    id,
                    journal_id,
                    Some(updated_at),
                    None,
                    &format!(r#"{{"id":"{id}"}}"#),
                )
                .expect("entry should save");
        }

        let output = entries_by_journal_id(
            &store,
            journal_id,
            PaginationInput {
                limit: Some(1),
                offset: None,
                cursor: None,
            },
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
            None,
        )
        .expect("paginated list should work");

        assert_eq!(output.count, 1);
        assert_eq!(output.limit, Some(1));
        assert_eq!(output.offset, None);
        assert!(output.has_more);
        assert!(output.next_cursor.is_some());
    }
}
