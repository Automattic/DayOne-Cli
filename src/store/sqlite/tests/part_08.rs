use super::common::test_store_path;
use crate::store::sqlite::{ListPageRequest, Store};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::json;

#[test]
fn comment_rows_round_trip_and_paginate_by_entry() {
    let path = test_store_path("comments-pagination");
    let store = Store::open_at(&*path).expect("store should open");

    for i in 0..3 {
        let comment = json!({
            "id": format!("comment-{i}"),
            "journal_id": "journal-1",
            "entry_id": "entry-1",
            "author_id": "user-1",
            "created_at": format!("2026-04-0{}T00:00:00.000Z", i + 1),
            "updated_at": "",
            "deleted_at": "",
            "content": format!("message-{i}")
        });
        store
            .upsert_comment_json_row(&comment.to_string())
            .expect("comment should upsert");
    }

    let first_page = store
        .list_comments_json_rows_by_entry_paged(
            "journal-1",
            "entry-1",
            &ListPageRequest {
                limit: Some(2),
                offset: None,
                cursor: None,
            },
        )
        .expect("first page should load");
    assert_eq!(first_page.rows.len(), 2);
    assert!(first_page.has_more);
    assert!(first_page.next_cursor.is_some());

    let second_page = store
        .list_comments_json_rows_by_entry_paged(
            "journal-1",
            "entry-1",
            &ListPageRequest {
                limit: Some(2),
                offset: None,
                cursor: first_page.next_cursor.clone(),
            },
        )
        .expect("second page should load");
    assert_eq!(second_page.rows.len(), 1);
    assert!(!second_page.has_more);
}

#[test]
fn comment_pagination_skips_rows_missing_created_at() {
    let path = test_store_path("comments-pagination-skip-missing-created-at");
    let store = Store::open_at(&*path).expect("store should open");

    store
        .upsert_comment_json_row(
            &json!({
                "id": "comment-missing-created-at",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "content": "missing timestamp"
            })
            .to_string(),
        )
        .expect("comment without created_at should still upsert");

    for i in 0..2 {
        let comment = json!({
            "id": format!("comment-{i}"),
            "journal_id": "journal-1",
            "entry_id": "entry-1",
            "author_id": "user-1",
            "created_at": format!("2026-04-0{}T00:00:00.000Z", i + 1),
            "content": format!("message-{i}")
        });
        store
            .upsert_comment_json_row(&comment.to_string())
            .expect("comment should upsert");
    }

    let first_page = store
        .list_comments_json_rows_by_entry_paged(
            "journal-1",
            "entry-1",
            &ListPageRequest {
                limit: Some(1),
                offset: None,
                cursor: None,
            },
        )
        .expect("first page should load");
    assert_eq!(first_page.rows.len(), 1);
    assert!(first_page.has_more);
    assert!(first_page.next_cursor.is_some());

    let second_page = store
        .list_comments_json_rows_by_entry_paged(
            "journal-1",
            "entry-1",
            &ListPageRequest {
                limit: Some(1),
                offset: None,
                cursor: first_page.next_cursor.clone(),
            },
        )
        .expect("second page should load with generated cursor");
    assert_eq!(second_page.rows.len(), 1);
    assert!(!second_page.has_more);
}

#[test]
fn comment_reactions_upsert_and_delete_by_user() {
    let path = test_store_path("comment-reactions");
    let store = Store::open_at(&*path).expect("store should open");

    let reaction = json!({
        "id": "reaction-1",
        "journal_id": "journal-1",
        "entry_id": "entry-1",
        "comment_id": "comment-1",
        "user_id": "user-1",
        "reaction": "like",
        "timestamp": "2026-04-01T00:00:00.000Z"
    });
    store
        .upsert_comment_reaction_json_row(&reaction.to_string())
        .expect("reaction should upsert");

    let rows = store
        .list_comment_reactions_json_rows_by_comment("journal-1", "entry-1", "comment-1")
        .expect("reactions should list");
    assert_eq!(rows.len(), 1);

    store
        .delete_comment_reaction_by_user("journal-1", "entry-1", "comment-1", "user-1")
        .expect("reaction delete should succeed");
    let rows = store
        .list_comment_reactions_json_rows_by_comment("journal-1", "entry-1", "comment-1")
        .expect("reactions should list after delete");
    assert!(rows.is_empty());
}

#[test]
fn remap_comment_local_id_handles_existing_target_id() {
    let path = test_store_path("comment-remap-existing-target");
    let store = Store::open_at(&*path).expect("store should open");
    store
        .upsert_comment_json_row(
            &json!({
                "id": "local-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "content": "pending"
            })
            .to_string(),
        )
        .expect("local comment should save");
    store
        .upsert_comment_json_row(
            &json!({
                "id": "server-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "content": "server"
            })
            .to_string(),
        )
        .expect("server comment should save");
    store
        .enqueue_outbox_item(
            "comment:update:journal-1:entry-1:local-1",
            "comment",
            "update",
            "journal-1:entry-1:local-1",
            &json!({
                "id": "local-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "content": "pending"
            })
            .to_string(),
            0,
        )
        .expect("outbox should enqueue");

    store
        .remap_comment_local_id("journal-1", "entry-1", "local-1", "server-1")
        .expect("remap should succeed");
    let local = store
        .get_comment_json_row_by_id("local-1")
        .expect("read should succeed");
    assert!(
        local.is_none(),
        "local placeholder should be gone after remap"
    );

    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("outbox lease should succeed");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].object_id, "journal-1:entry-1:server-1");
    let payload: serde_json::Value =
        serde_json::from_str(&leased[0].payload_json).expect("payload should parse");
    assert_eq!(
        payload.get("id").and_then(serde_json::Value::as_str),
        Some("server-1")
    );
}

#[test]
fn comment_reaction_upsert_replaces_identity_with_new_id() {
    let path = test_store_path("comment-reaction-identity-replace");
    let store = Store::open_at(&*path).expect("store should open");
    store
        .upsert_comment_reaction_json_row(
            &json!({
                "id": "reaction-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "comment_id": "comment-1",
                "user_id": "user-1",
                "reaction": "like"
            })
            .to_string(),
        )
        .expect("first reaction should upsert");
    store
        .upsert_comment_reaction_json_row(
            &json!({
                "id": "reaction-2",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "comment_id": "comment-1",
                "user_id": "user-1",
                "reaction": "love"
            })
            .to_string(),
        )
        .expect("second reaction should upsert");

    let rows = store
        .list_comment_reactions_json_rows_by_comment("journal-1", "entry-1", "comment-1")
        .expect("reactions should list");
    assert_eq!(rows.len(), 1, "identity key should remain unique");
    let payload: serde_json::Value = serde_json::from_str(&rows[0]).expect("row should parse");
    assert_eq!(
        payload.get("id").and_then(serde_json::Value::as_str),
        Some("reaction-2")
    );
    assert_eq!(
        payload.get("reaction").and_then(serde_json::Value::as_str),
        Some("love")
    );
}

#[test]
fn remap_comment_local_id_does_not_mutate_processing_outbox_payload() {
    let path = test_store_path("comment-remap-processing-outbox");
    let store = Store::open_at(&*path).expect("store should open");
    store
        .upsert_comment_json_row(
            &json!({
                "id": "local-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "content": "pending"
            })
            .to_string(),
        )
        .expect("local comment should save");
    store
        .enqueue_outbox_item(
            "comment:create:journal-1:entry-1:local-1",
            "comment",
            "create",
            "journal-1:entry-1:local-1",
            &json!({
                "id": "local-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "content": "pending"
            })
            .to_string(),
            0,
        )
        .expect("outbox should enqueue");
    let leased = store
        .lease_outbox_items(i64::MAX, 1)
        .expect("outbox should lease one row");
    let leased_item = leased.first().cloned().expect("leased row should exist");
    let leased_payload_json = leased_item.payload_json.clone();

    store
        .remap_comment_local_id("journal-1", "entry-1", "local-1", "server-1")
        .expect("remap should succeed");
    store
        .mark_outbox_item_succeeded(&leased_item.id, &leased_payload_json)
        .expect("processing row should still be deletable by leased payload");

    let remaining = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("outbox lease should succeed");
    assert!(
        remaining.is_empty(),
        "outbox row should be removed, not reverted to pending"
    );
}

#[test]
fn remap_comment_local_id_updates_embedded_json_ids() {
    let path = test_store_path("comment-remap-updates-json-ids");
    let store = Store::open_at(&*path).expect("store should open");
    store
        .upsert_comment_json_row(
            &json!({
                "id": "local-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "content": "pending"
            })
            .to_string(),
        )
        .expect("local comment should save");
    store
        .upsert_comment_reaction_json_row(
            &json!({
                "id": "reaction-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "comment_id": "local-1",
                "user_id": "user-1",
                "reaction": "like"
            })
            .to_string(),
        )
        .expect("reaction should save");

    store
        .remap_comment_local_id("journal-1", "entry-1", "local-1", "server-1")
        .expect("remap should succeed");

    let comment_json = store
        .get_comment_json_row_by_id("server-1")
        .expect("comment read should succeed")
        .expect("server remapped comment should exist");
    let comment: serde_json::Value =
        serde_json::from_str(&comment_json).expect("comment json should parse");
    assert_eq!(
        comment.get("id").and_then(serde_json::Value::as_str),
        Some("server-1"),
        "embedded comment id should be remapped"
    );

    let reactions = store
        .list_comment_reactions_json_rows_by_comment("journal-1", "entry-1", "server-1")
        .expect("reaction read should succeed");
    assert_eq!(
        reactions.len(),
        1,
        "reaction should follow remapped comment id"
    );
    let reaction: serde_json::Value =
        serde_json::from_str(&reactions[0]).expect("reaction json should parse");
    assert_eq!(
        reaction
            .get("comment_id")
            .and_then(serde_json::Value::as_str),
        Some("server-1"),
        "embedded reaction comment_id should be remapped"
    );
}

#[test]
fn upsert_comment_json_row_rejects_empty_required_ids() {
    let path = test_store_path("comment-reject-empty-required-ids");
    let store = Store::open_at(&*path).expect("store should open");
    let err = store
        .upsert_comment_json_row(
            &json!({
                "id": "   ",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "content": "hello"
            })
            .to_string(),
        )
        .expect_err("empty id should fail");
    assert!(
        err.to_string().contains("missing or empty id"),
        "unexpected error: {err}"
    );
}

#[test]
fn upsert_comment_reaction_json_row_rejects_empty_required_fields() {
    let path = test_store_path("comment-reaction-reject-empty-required-fields");
    let store = Store::open_at(&*path).expect("store should open");
    let err = store
        .upsert_comment_reaction_json_row(
            &json!({
                "id": "reaction-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "comment_id": "  ",
                "user_id": "user-1",
                "reaction": "like"
            })
            .to_string(),
        )
        .expect_err("empty comment_id should fail");
    assert!(
        err.to_string().contains("missing or empty comment_id"),
        "unexpected error: {err}"
    );
}

#[test]
fn list_comment_reactions_by_comment_ids_supports_large_batches() {
    let path = test_store_path("comment-reaction-large-batch-lookup");
    let store = Store::open_at(&*path).expect("store should open");
    let mut comment_ids = Vec::new();
    for i in 0..1200 {
        let comment_id = format!("comment-{i}");
        comment_ids.push(comment_id.clone());
        store
            .upsert_comment_reaction_json_row(
                &json!({
                    "id": format!("reaction-{i}"),
                    "journal_id": "journal-1",
                    "entry_id": "entry-1",
                    "comment_id": comment_id,
                    "user_id": format!("user-{i}"),
                    "reaction": "like"
                })
                .to_string(),
            )
            .expect("reaction should upsert");
    }

    let rows = store
        .list_comment_reactions_json_rows_by_comment_ids("journal-1", "entry-1", &comment_ids)
        .expect("batched reaction lookup should succeed");
    assert_eq!(rows.len(), 1200, "all reaction rows should be returned");
}

#[test]
fn comment_row_with_non_string_deleted_at_is_treated_as_deleted() {
    let path = test_store_path("comment-non-string-deleted-at");
    let store = Store::open_at(&*path).expect("store should open");
    store
        .upsert_comment_json_row(
            &json!({
                "id": "comment-deleted",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "created_at": "2026-04-01T00:00:00.000Z",
                "deletedAt": { "unexpected": true },
                "content": "should be hidden"
            })
            .to_string(),
        )
        .expect("comment should upsert");

    let page = store
        .list_comments_json_rows_by_entry_paged(
            "journal-1",
            "entry-1",
            &ListPageRequest {
                limit: Some(10),
                offset: None,
                cursor: None,
            },
        )
        .expect("comment page should load");
    assert!(
        page.rows.is_empty(),
        "comment should be filtered as deleted from list results"
    );
}

#[test]
fn comment_list_cursor_rejects_legacy_updated_at_payload() {
    let path = test_store_path("comment-list-cursor-rejects-legacy-updated-at");
    let store = Store::open_at(&*path).expect("store should open");
    store
        .upsert_comment_json_row(
            &json!({
                "id": "comment-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "created_at": "2026-04-01T00:00:00.000Z",
                "content": "one"
            })
            .to_string(),
        )
        .expect("comment 1 should upsert");
    store
        .upsert_comment_json_row(
            &json!({
                "id": "comment-2",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "created_at": "2026-04-02T00:00:00.000Z",
                "content": "two"
            })
            .to_string(),
        )
        .expect("comment 2 should upsert");

    let legacy_cursor = BASE64_STANDARD.encode(
        json!({
            "updated_at": "2026-04-01T00:00:00.000Z",
            "id": "comment-1"
        })
        .to_string(),
    );
    let err = store
        .list_comments_json_rows_by_entry_paged(
            "journal-1",
            "entry-1",
            &ListPageRequest {
                limit: Some(10),
                offset: None,
                cursor: Some(legacy_cursor),
            },
        )
        .expect_err("legacy updated_at cursor should be rejected");
    assert!(
        err.to_string().contains("expected encoded JSON token"),
        "unexpected error: {err}"
    );
}

#[test]
fn outbox_evidence_matches_entry_segment_only() {
    let path = test_store_path("outbox-evidence-entry-segment");
    let store = Store::open_at(&*path).expect("store should open");

    // A comment row whose *child* id collides with the entry id we probe for.
    store
        .enqueue_outbox_item(
            "comment:update:journal-1:entry-parent:entry-child",
            "comment",
            "update",
            "journal-1:entry-parent:entry-child",
            "{}",
            0,
        )
        .expect("comment outbox should enqueue");

    assert!(
        !store
            .has_outbox_evidence_for_entry("entry-child")
            .expect("evidence query should succeed"),
        "a child id equal to the probed entry id must not count as evidence"
    );
    assert!(
        store
            .has_outbox_evidence_for_entry("entry-parent")
            .expect("evidence query should succeed"),
        "the entry segment of a child row must count as evidence"
    );

    // Plain entry row, later probed as if the entry moved to another journal.
    store
        .enqueue_outbox_item(
            "entry:journal-1:entry-moved",
            "entry",
            "update",
            "journal-1:entry-moved",
            "{}",
            0,
        )
        .expect("entry outbox should enqueue");
    assert!(
        store
            .has_outbox_evidence_for_entry("entry-moved")
            .expect("evidence query should succeed"),
        "entry rows must match regardless of the journal they were queued under"
    );
    assert!(
        !store
            .has_outbox_evidence_for_entry("entry-absent")
            .expect("evidence query should succeed"),
        "an entry with no outbox rows has no evidence"
    );
}

#[test]
fn settling_entry_outbox_preserves_failed_update_for_recovery() {
    let path = test_store_path("outbox-preserve-failed-entry-update");
    let store = Store::open_at(&*path).expect("store should open");
    let object_id = "journal-1:entry-1";

    for id in ["entry:failed", "entry:pending"] {
        store
            .enqueue_outbox_item(
                id,
                "entry",
                "update",
                object_id,
                r#"{"body":"local edit"}"#,
                0,
            )
            .expect("entry outbox should enqueue");
    }
    store
        .connect()
        .expect("connection should open")
        .execute(
            "UPDATE sync_outbox SET status = 'failed' WHERE id = 'entry:failed'",
            [],
        )
        .expect("failed status should save");

    assert!(
        store
            .has_inflight_entry_update("journal-1", "entry-1")
            .expect("inflight query should succeed")
    );
    assert_eq!(
        store
            .delete_settled_entry_outbox_for_entry("journal-1", "entry-1")
            .expect("settled outbox delete should succeed"),
        1
    );

    let remaining = store
        .list_outbox_item_details(true)
        .expect("outbox should list");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, "entry:failed");
    assert_eq!(remaining[0].status, "failed");
    assert!(remaining[0].payload_json.is_some());
    assert!(
        !store
            .has_inflight_entry_update("journal-1", "entry-1")
            .expect("inflight query should succeed")
    );
}
