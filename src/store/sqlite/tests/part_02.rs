use super::super::*;
use super::common::test_store_path;
use crate::store::json_rows::encode_list_cursor;
use serde_json::Value;

#[test]
fn list_entries_paged_cursor_uses_iso_entry_date_order() {
    let path = test_store_path("entries-cursor-iso-date");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-cursor-iso";
    for (id, updated_at, date) in [
        (
            "entry-z-earliest",
            "2026-03-01T00:00:00.000Z",
            "2026-03-01T00:00:00.000Z",
        ),
        (
            "entry-m-middle",
            "2026-03-02T00:00:00.000Z",
            "2026-03-02T00:00:00.000Z",
        ),
        (
            "entry-a-latest",
            "2026-03-03T00:00:00.000Z",
            "2026-03-03T00:00:00.000Z",
        ),
    ] {
        store
            .upsert_entry_json_row(
                id,
                journal_id,
                Some(updated_at),
                None,
                &format!(r#"{{"id":"{id}","date":"{date}"}}"#),
            )
            .expect("entry should save");
    }

    let first_page = ListPageRequest {
        limit: Some(1),
        offset: None,
        cursor: None,
    };
    let first = store
        .list_entries_json_rows_by_journal_id_paged(
            journal_id,
            &first_page,
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
        )
        .expect("first page should list");
    assert_eq!(first.rows.len(), 1);
    let first_id = serde_json::from_str::<Value>(&first.rows[0])
        .expect("row should be json")
        .get("id")
        .and_then(Value::as_str)
        .expect("row should contain id")
        .to_owned();
    assert_eq!(first_id, "entry-a-latest");
    let next_cursor = first.next_cursor.expect("next cursor should exist");

    let second_page = ListPageRequest {
        limit: Some(1),
        offset: None,
        cursor: Some(next_cursor),
    };
    let second = store
        .list_entries_json_rows_by_journal_id_paged(
            journal_id,
            &second_page,
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
        )
        .expect("second page should list");
    assert_eq!(second.rows.len(), 1);
    let second_id = serde_json::from_str::<Value>(&second.rows[0])
        .expect("row should be json")
        .get("id")
        .and_then(Value::as_str)
        .expect("row should contain id")
        .to_owned();
    assert_eq!(second_id, "entry-m-middle");
}

#[test]
fn paged_listing_rejects_invalid_cursor() {
    let path = test_store_path("invalid-cursor");
    let store = Store::open_at(&path).expect("store should open");
    let page = ListPageRequest {
        limit: Some(10),
        offset: None,
        cursor: Some("not-a-valid-cursor".to_owned()),
    };
    let err = store
        .list_json_rows_paged(ListJsonTable::DailyChatFeed, &page)
        .expect_err("invalid cursor should fail");
    assert!(
        err.to_string().contains("cursor is invalid"),
        "unexpected error: {err}"
    );
}

#[test]
fn list_entries_orders_by_user_edit_date_desc_when_requested() {
    let path = test_store_path("entries-order-edit-date");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-edit-date";

    store
        .upsert_entry_json_row_with_edit_date(
            "entry-a-later-entry-date",
            journal_id,
            Some("2026-03-01T10:00:00.000Z"),
            Some("2026-03-01T10:00:00.000Z"),
            None,
            r#"{"id":"entry-a-later-entry-date","date":2000}"#,
        )
        .expect("first entry should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-b-later-edit-date",
            journal_id,
            Some("2026-03-02T10:00:00.000Z"),
            Some("2026-03-02T10:00:00.000Z"),
            None,
            r#"{"id":"entry-b-later-edit-date","date":1000}"#,
        )
        .expect("second entry should save");

    let rows = store
        .list_entries_json_rows_by_journal_id(
            journal_id,
            None,
            EntryListSort::Desc,
            EntryListSortMethod::EditDate,
        )
        .expect("entries should list");
    let ids: Vec<String> = rows
        .iter()
        .map(|row| {
            serde_json::from_str::<Value>(row)
                .expect("row should be json")
                .get("id")
                .and_then(Value::as_str)
                .expect("row should contain id")
                .to_owned()
        })
        .collect();

    assert_eq!(
        ids,
        vec![
            "entry-b-later-edit-date".to_owned(),
            "entry-a-later-entry-date".to_owned()
        ]
    );
}

#[test]
fn list_entries_edit_date_handles_unparseable_text_as_zero() {
    let path = test_store_path("entries-edit-date-unparseable");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-edit-date-unparseable";

    store
        .upsert_entry_json_row_with_edit_date(
            "entry-b-invalid-edit-date",
            journal_id,
            Some("2026-03-01T10:00:00.000Z"),
            Some("not-a-date"),
            None,
            r#"{"id":"entry-b-invalid-edit-date","date":2000}"#,
        )
        .expect("entry should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-a-valid-edit-date",
            journal_id,
            Some("2026-03-02T10:00:00.000Z"),
            Some("2026-03-02T10:00:00.000Z"),
            None,
            r#"{"id":"entry-a-valid-edit-date","date":1000}"#,
        )
        .expect("entry should save");

    let rows = store
        .list_entries_json_rows_by_journal_id(
            journal_id,
            None,
            EntryListSort::Desc,
            EntryListSortMethod::EditDate,
        )
        .expect("entries should list");
    let ids: Vec<String> = rows
        .iter()
        .map(|row| {
            serde_json::from_str::<Value>(row)
                .expect("row should be json")
                .get("id")
                .and_then(Value::as_str)
                .expect("row should contain id")
                .to_owned()
        })
        .collect();

    assert_eq!(
        ids,
        vec![
            "entry-a-valid-edit-date".to_owned(),
            "entry-b-invalid-edit-date".to_owned()
        ]
    );
}

#[test]
fn list_entries_paged_rejects_non_numeric_cursor_sort_key() {
    let path = test_store_path("entries-cursor-non-numeric-sort-key");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-cursor-numeric-check";
    store
        .upsert_entry_json_row(
            "entry-1",
            journal_id,
            Some("2026-03-01T00:00:00.000Z"),
            None,
            r#"{"id":"entry-1","date":"2026-03-01T00:00:00.000Z"}"#,
        )
        .expect("entry should save");

    let page = ListPageRequest {
        limit: Some(10),
        offset: None,
        cursor: Some(encode_list_cursor("2026-03-01T00:00:00.000Z", "entry-1")),
    };
    let err = store
        .list_entries_json_rows_by_journal_id_paged(
            journal_id,
            &page,
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
        )
        .expect_err("non-numeric cursor sort key should fail");
    assert!(
        err.to_string().contains("expected numeric sort key"),
        "unexpected error: {err}"
    );
}

#[test]
fn paged_listing_rejects_zero_limit() {
    let path = test_store_path("zero-limit");
    let store = Store::open_at(&path).expect("store should open");
    let page = ListPageRequest {
        limit: Some(0),
        offset: None,
        cursor: None,
    };
    let err = store
        .list_json_rows_paged(ListJsonTable::DailyChatFeed, &page)
        .expect_err("zero limit should fail");
    assert!(
        err.to_string().contains("at least 1"),
        "unexpected error: {err}"
    );
}

#[test]
fn paged_listing_rejects_offset_and_cursor_together() {
    let path = test_store_path("offset-and-cursor");
    let store = Store::open_at(&path).expect("store should open");
    let page = ListPageRequest {
        limit: Some(10),
        offset: Some(1),
        cursor: Some("abc".to_owned()),
    };
    let err = store
        .list_json_rows_paged(ListJsonTable::DailyChatFeed, &page)
        .expect_err("offset+cursor should fail");
    assert!(
        err.to_string().contains("cannot be used together"),
        "unexpected error: {err}"
    );
}

#[test]
fn list_journals_excludes_deleted_by_default() {
    let path = test_store_path("journals-exclude-deleted-default");
    let store = Store::open_at(&path).expect("store should open");

    store
        .upsert_json_row(
            "journals",
            "journal-active-1",
            Some("2026-03-03T00:00:00.000Z"),
            None,
            r#"{"id":"journal-active-1","state":"active"}"#,
        )
        .expect("active journal should save");
    store
        .upsert_json_row(
            "journals",
            "journal-state-deleted",
            Some("2026-03-02T00:00:00.000Z"),
            None,
            r#"{"id":"journal-state-deleted","state":"deleted"}"#,
        )
        .expect("state-deleted journal should save");
    store
        .upsert_json_row(
            "journals",
            "journal-tombstone-deleted",
            Some("2026-03-01T00:00:00.000Z"),
            Some("2026-03-04T00:00:00.000Z"),
            r#"{"id":"journal-tombstone-deleted","state":"active"}"#,
        )
        .expect("tombstone-deleted journal should save");

    let page = ListPageRequest::default();
    let result = store
        .list_journals_json_rows_paged(&page, false)
        .expect("journals should list");

    let ids: Vec<String> = result
        .rows
        .iter()
        .map(|row| {
            serde_json::from_str::<Value>(row)
                .expect("row should be json")
                .get("id")
                .and_then(Value::as_str)
                .expect("row should contain id")
                .to_owned()
        })
        .collect();
    assert_eq!(ids, vec!["journal-active-1".to_owned()]);
}

#[test]
fn list_journals_include_deleted_returns_deleted_rows() {
    let path = test_store_path("journals-include-deleted");
    let store = Store::open_at(&path).expect("store should open");

    for (id, updated_at, deleted_at, state) in [
        ("journal-active", "2026-03-03T00:00:00.000Z", None, "active"),
        (
            "journal-state-deleted",
            "2026-03-02T00:00:00.000Z",
            None,
            "deleted",
        ),
        (
            "journal-tombstone-deleted",
            "2026-03-01T00:00:00.000Z",
            Some("2026-03-04T00:00:00.000Z"),
            "active",
        ),
    ] {
        store
            .upsert_json_row(
                "journals",
                id,
                Some(updated_at),
                deleted_at,
                &format!(r#"{{"id":"{id}","state":"{state}"}}"#),
            )
            .expect("journal should save");
    }

    let page = ListPageRequest::default();
    let result = store
        .list_journals_json_rows_paged(&page, true)
        .expect("journals should list");
    let ids: Vec<String> = result
        .rows
        .iter()
        .map(|row| {
            serde_json::from_str::<Value>(row)
                .expect("row should be json")
                .get("id")
                .and_then(Value::as_str)
                .expect("row should contain id")
                .to_owned()
        })
        .collect();

    assert_eq!(
        ids,
        vec![
            "journal-active".to_owned(),
            "journal-state-deleted".to_owned(),
            "journal-tombstone-deleted".to_owned()
        ]
    );
}

#[test]
fn list_journals_pagination_counts_only_visible_rows_by_default() {
    let path = test_store_path("journals-pagination-visible-only");
    let store = Store::open_at(&path).expect("store should open");

    for (id, updated_at, deleted_at, state) in [
        (
            "journal-active-1",
            "2026-03-03T00:00:00.000Z",
            None,
            "active",
        ),
        (
            "journal-state-deleted",
            "2026-03-02T00:00:00.000Z",
            None,
            "deleted",
        ),
        (
            "journal-active-2",
            "2026-03-01T00:00:00.000Z",
            None,
            "active",
        ),
    ] {
        store
            .upsert_json_row(
                "journals",
                id,
                Some(updated_at),
                deleted_at,
                &format!(r#"{{"id":"{id}","state":"{state}"}}"#),
            )
            .expect("journal should save");
    }

    let first_page = ListPageRequest {
        limit: Some(1),
        offset: None,
        cursor: None,
    };
    let first = store
        .list_journals_json_rows_paged(&first_page, false)
        .expect("first page should list");
    assert_eq!(first.rows.len(), 1);
    assert!(first.has_more);
    let first_id = serde_json::from_str::<Value>(&first.rows[0])
        .expect("row should be json")
        .get("id")
        .and_then(Value::as_str)
        .expect("row should contain id")
        .to_owned();
    assert_eq!(first_id, "journal-active-1");
    let cursor = first.next_cursor.expect("next cursor should exist");

    let second_page = ListPageRequest {
        limit: Some(1),
        offset: None,
        cursor: Some(cursor),
    };
    let second = store
        .list_journals_json_rows_paged(&second_page, false)
        .expect("second page should list");
    assert_eq!(second.rows.len(), 1);
    assert!(!second.has_more);
    assert!(second.next_cursor.is_none());
    let second_id = serde_json::from_str::<Value>(&second.rows[0])
        .expect("row should be json")
        .get("id")
        .and_then(Value::as_str)
        .expect("row should contain id")
        .to_owned();
    assert_eq!(second_id, "journal-active-2");
}
