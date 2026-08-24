use super::super::*;
use super::common::test_store_path;
use rusqlite::params;

#[test]
fn sync_lock_blocks_reacquire_until_released() {
    let path = test_store_path("lock");
    let store = Store::open_at(&path).expect("store should open");
    let first = store
        .try_acquire_sync_lock(10_000)
        .expect("should acquire first lock");
    assert!(first);
    let second = store
        .try_acquire_sync_lock(10_000)
        .expect("should check second lock");
    assert!(!second);
    store.release_sync_lock().expect("should release lock");
    let third = store
        .try_acquire_sync_lock(10_000)
        .expect("should acquire after release");
    assert!(third);
}

#[test]
fn entry_attachment_roundtrip_and_status_updates() {
    let path = test_store_path("entry-attachment");
    let store = Store::open_at(&path).expect("store should open");
    let attachment = EntryAttachmentUpsert {
        journal_id: "j1".to_owned(),
        entry_id: "e1".to_owned(),
        moment_id: "m1".to_owned(),
        file_path: "/tmp/test.jpg".to_owned(),
        moment_type: "image".to_owned(),
        content_type: "image/jpeg".to_owned(),
        file_size_bytes: 123,
        md5_body: "abc123".to_owned(),
        width: Some(100),
        height: Some(200),
        duration_seconds: None,
        thumbnail_content_type: Some("image/jpeg".to_owned()),
        thumbnail_md5: Some("thumb123".to_owned()),
        thumbnail_width: Some(50),
        thumbnail_height: Some(50),
        thumbnail_file_size_bytes: Some(10),
        thumbnail_bytes: Some(vec![1, 2, 3]),
    };
    store
        .upsert_entry_attachment(&attachment)
        .expect("attachment should upsert");
    let listed = store
        .list_entry_attachments("j1", "e1")
        .expect("attachments should list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].moment_id, "m1");
    assert_eq!(listed[0].thumbnail_bytes.as_deref(), Some(&[1, 2, 3][..]));

    store
        .mark_entry_attachments_associated("j1", "e1")
        .expect("attachment should mark associated");
    store
        .mark_entry_attachment_error("j1", "e1", "m1", "temporary error")
        .expect("attachment should persist error");
    store
        .mark_entry_attachment_original_uploaded("j1", "e1", "m1")
        .expect("attachment should mark uploaded");

    let updated = store
        .get_entry_attachment("j1", "e1", "m1")
        .expect("attachment query should work")
        .expect("attachment should exist");
    assert!(updated.entry_associated);
    assert!(updated.original_uploaded);
    assert!(updated.last_error.is_none());
}

#[test]
fn outbox_enqueue_lease_retry_success_lifecycle() {
    let path = test_store_path("outbox-lifecycle");
    let store = Store::open_at(&path).expect("store should open");
    store
        .enqueue_outbox_item("entry:j1:e1", "entry", "update", "j1:e1", "{}", 0)
        .expect("enqueue should work");
    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].resource, "entry");
    assert_eq!(leased[0].operation, "update");
    store
        .mark_outbox_item_retry("entry:j1:e1", "{}", i64::MAX, Some("retry"))
        .expect("retry mark should work");
    let none_due = store
        .lease_outbox_items(0, 10)
        .expect("lease should still work");
    assert!(none_due.is_empty());
    let due_again = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should find due item");
    assert_eq!(due_again.len(), 1);
    store
        .mark_outbox_item_succeeded("entry:j1:e1", "{}")
        .expect("success should delete");
    let empty = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert!(empty.is_empty());
}

#[test]
fn outbox_enqueue_if_not_pending_skips_duplicate_pending_rows() {
    let path = test_store_path("outbox-dedupe-pending");
    let store = Store::open_at(&path).expect("store should open");
    let payload_v1 = r#"{"date":"2026-03-17","value":"v1"}"#;
    let payload_v2 = r#"{"date":"2026-03-17","value":"v2"}"#;

    let first = store
        .enqueue_outbox_item_if_not_pending("daily_chat_feed", "update", "2026-03-17", payload_v1)
        .expect("first enqueue should work");
    assert!(first);

    let second = store
        .enqueue_outbox_item_if_not_pending("daily_chat_feed", "update", "2026-03-17", payload_v2)
        .expect("second enqueue should work");
    assert!(!second, "duplicate pending item should be skipped");

    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].id, "daily_chat_feed:update:2026-03-17");
    assert_eq!(leased[0].payload_json, payload_v1);
}

#[test]
fn outbox_enqueue_if_not_pending_skips_duplicate_pending_rows_even_with_legacy_id() {
    let path = test_store_path("outbox-dedupe-pending-legacy-id");
    let store = Store::open_at(&path).expect("store should open");
    let payload_v1 = r#"{"date":"2026-03-17","value":"legacy"}"#;
    let payload_v2 = r#"{"date":"2026-03-17","value":"new"}"#;

    store
        .enqueue_outbox_item(
            "daily_chat_feed:2026-03-17",
            "daily_chat_feed",
            "update",
            "2026-03-17",
            payload_v1,
            0,
        )
        .expect("legacy-style enqueue should work");

    let queued = store
        .enqueue_outbox_item_if_not_pending("daily_chat_feed", "update", "2026-03-17", payload_v2)
        .expect("enqueue_if_not_pending should work");
    assert!(
        !queued,
        "pending row with same object should block duplicate enqueue"
    );

    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].id, "daily_chat_feed:2026-03-17");
    assert_eq!(leased[0].payload_json, payload_v1);
}

#[test]
fn outbox_enqueue_if_not_pending_does_not_mutate_processing_lease_payload() {
    let path = test_store_path("outbox-dedupe-processing-lease");
    let store = Store::open_at(&path).expect("store should open");
    let payload_v1 = r#"{"date":"2026-03-17","value":"leased"}"#;
    let payload_v2 = r#"{"date":"2026-03-17","value":"should-not-overwrite"}"#;

    store
        .enqueue_outbox_item(
            "daily_chat_feed:legacy",
            "daily_chat_feed",
            "update",
            "2026-03-17",
            payload_v1,
            0,
        )
        .expect("seed enqueue should work");

    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].id, "daily_chat_feed:legacy");
    assert_eq!(leased[0].payload_json, payload_v1);

    let queued = store
        .enqueue_outbox_item_if_not_pending("daily_chat_feed", "update", "2026-03-17", payload_v2)
        .expect("enqueue_if_not_pending should work");
    assert!(
        !queued,
        "processing row with same object should block duplicate enqueue"
    );

    // If enqueue_if_not_pending mutates payload_json for a processing lease, this delete
    // predicate will miss and leave a pending row behind.
    store
        .mark_outbox_item_succeeded(&leased[0].id, &leased[0].payload_json)
        .expect("success mark should delete processing row");

    let none_due = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert!(
        none_due.is_empty(),
        "processing row should have been removed"
    );
}

#[test]
fn outbox_enqueue_if_not_pending_requeues_after_failure() {
    let path = test_store_path("outbox-dedupe-requeue");
    let store = Store::open_at(&path).expect("store should open");
    let payload_v1 = r#"{"date":"2026-03-17","value":"v1"}"#;
    let payload_v2 = r#"{"date":"2026-03-17","value":"v2"}"#;

    let first = store
        .enqueue_outbox_item_if_not_pending("daily_chat_feed", "update", "2026-03-17", payload_v1)
        .expect("first enqueue should work");
    assert!(first);

    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);

    store
        .mark_outbox_item_failed(
            &leased[0].id,
            &leased[0].payload_json,
            8,
            Some("forced test failure"),
        )
        .expect("mark failed should work");

    let requeued = store
        .enqueue_outbox_item_if_not_pending("daily_chat_feed", "update", "2026-03-17", payload_v2)
        .expect("requeue should work");
    assert!(requeued, "failed rows should be enqueueable again");

    let leased_again = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased_again.len(), 1);
    assert_eq!(leased_again[0].payload_json, payload_v2);
}

#[test]
fn outbox_enqueue_if_not_pending_revives_failed_legacy_row_in_place() {
    let path = test_store_path("outbox-dedupe-revive-legacy-failed");
    let store = Store::open_at(&path).expect("store should open");
    let payload_v1 = r#"{"date":"2026-03-17","value":"legacy-failed"}"#;
    let payload_v2 = r#"{"date":"2026-03-17","value":"revived"}"#;

    store
        .enqueue_outbox_item(
            "daily_chat_feed:legacy-failed",
            "daily_chat_feed",
            "update",
            "2026-03-17",
            payload_v1,
            0,
        )
        .expect("legacy enqueue should work");

    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);

    store
        .mark_outbox_item_failed(
            &leased[0].id,
            &leased[0].payload_json,
            8,
            Some("forced failure"),
        )
        .expect("mark failed should work");

    let queued = store
        .enqueue_outbox_item_if_not_pending("daily_chat_feed", "update", "2026-03-17", payload_v2)
        .expect("enqueue_if_not_pending should work");
    assert!(queued, "failed tuple should be revived");

    let conn = store.connect().expect("connection should open");
    let row_count: i64 = conn
        .query_row(
            r#"
            SELECT COUNT(*)
            FROM sync_outbox
            WHERE resource = 'daily_chat_feed'
              AND operation = 'update'
              AND object_id = '2026-03-17'
            "#,
            [],
            |row| row.get(0),
        )
        .expect("count query should work");
    assert_eq!(
        row_count, 1,
        "should revive existing row, not insert duplicate"
    );

    let leased_again = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased_again.len(), 1);
    assert_eq!(leased_again[0].id, "daily_chat_feed:legacy-failed");
    assert_eq!(leased_again[0].payload_json, payload_v2);
}

#[test]
fn original_media_outbox_waits_until_attachment_is_associated() {
    let path = test_store_path("outbox-original-media-association");
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_entry_attachment(&EntryAttachmentUpsert {
            journal_id: "j1".to_owned(),
            entry_id: "e1".to_owned(),
            moment_id: "m1".to_owned(),
            file_path: "/tmp/test.jpg".to_owned(),
            moment_type: "image".to_owned(),
            content_type: "image/jpeg".to_owned(),
            file_size_bytes: 123,
            md5_body: "abc123".to_owned(),
            width: None,
            height: None,
            duration_seconds: None,
            thumbnail_content_type: None,
            thumbnail_md5: None,
            thumbnail_width: None,
            thumbnail_height: None,
            thumbnail_file_size_bytes: None,
            thumbnail_bytes: None,
        })
        .expect("attachment should upsert");
    store
        .enqueue_outbox_item(
            "original_media:j1:e1:m1",
            "original_media",
            "create",
            "j1:e1:m1",
            r#"{"journal_id":"j1","entry_id":"e1","moment_id":"m1"}"#,
            0,
        )
        .expect("original media enqueue should work");

    let blocked = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert!(blocked.is_empty());

    store
        .mark_entry_attachments_associated("j1", "e1")
        .expect("attachment association mark should work");
    let released = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].resource, "original_media");
}

#[test]
fn delete_original_media_outbox_for_entry_uses_literal_prefix_matching() {
    let path = test_store_path("outbox-delete-original-media-prefix");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j1";
    let entry_id = "e1";
    store
        .upsert_entry_attachment(&EntryAttachmentUpsert {
            journal_id: journal_id.to_owned(),
            entry_id: entry_id.to_owned(),
            moment_id: "m1".to_owned(),
            file_path: "/tmp/target.jpg".to_owned(),
            moment_type: "image".to_owned(),
            content_type: "image/jpeg".to_owned(),
            file_size_bytes: 123,
            md5_body: "abc123".to_owned(),
            width: None,
            height: None,
            duration_seconds: None,
            thumbnail_content_type: None,
            thumbnail_md5: None,
            thumbnail_width: None,
            thumbnail_height: None,
            thumbnail_file_size_bytes: None,
            thumbnail_bytes: None,
        })
        .expect("target attachment should upsert");
    store
        .upsert_entry_attachment(&EntryAttachmentUpsert {
            journal_id: "j10".to_owned(),
            entry_id: "e10".to_owned(),
            moment_id: "m1".to_owned(),
            file_path: "/tmp/other.jpg".to_owned(),
            moment_type: "image".to_owned(),
            content_type: "image/jpeg".to_owned(),
            file_size_bytes: 123,
            md5_body: "def456".to_owned(),
            width: None,
            height: None,
            duration_seconds: None,
            thumbnail_content_type: None,
            thumbnail_md5: None,
            thumbnail_width: None,
            thumbnail_height: None,
            thumbnail_file_size_bytes: None,
            thumbnail_bytes: None,
        })
        .expect("other attachment should upsert");
    store
        .enqueue_outbox_item(
            "original_media:target:m1",
            "original_media",
            "create",
            &format!("{journal_id}:{entry_id}:m1"),
            r#"{"journal_id":"j1","entry_id":"e1","moment_id":"m1"}"#,
            0,
        )
        .expect("target enqueue should work");
    store
        .enqueue_outbox_item(
            "original_media:other:m1",
            "original_media",
            "create",
            "j10:e10:m1",
            r#"{"journal_id":"j10","entry_id":"e10","moment_id":"m1"}"#,
            0,
        )
        .expect("other enqueue should work");
    let conn = store.connect().expect("connection should open");
    let total_original_media: i64 = conn
        .query_row(
            r#"
            SELECT count(*)
            FROM sync_outbox
            WHERE resource = 'original_media' AND operation = 'create'
            "#,
            [],
            |row| row.get(0),
        )
        .expect("count query should work");
    assert_eq!(total_original_media, 2, "both outbox rows should exist");
    let all_object_ids: String = conn
        .query_row(
            r#"
            SELECT group_concat(object_id, ',')
            FROM sync_outbox
            WHERE resource = 'original_media' AND operation = 'create'
            "#,
            [],
            |row| row.get(0),
        )
        .expect("object ids query should work");
    assert!(
        all_object_ids.contains("j1:e1:m1"),
        "target object_id missing from rows: {all_object_ids}"
    );
    let deletable_count: i64 = conn
        .query_row(
            r#"
            SELECT count(*)
            FROM sync_outbox
            WHERE resource = 'original_media'
              AND operation = 'create'
              AND instr(object_id, ?1) = 1
            "#,
            params![format!("{journal_id}:{entry_id}:")],
            |row| row.get(0),
        )
        .expect("deletable query should work");
    assert_eq!(
        deletable_count, 1,
        "expected one matching row before delete"
    );

    let _ = store
        .delete_original_media_outbox_for_entry(journal_id, entry_id)
        .expect("delete helper should work");

    let conn = store.connect().expect("connection should open");
    let remaining_count: i64 = conn
        .query_row(
            r#"
            SELECT count(*)
            FROM sync_outbox
            WHERE resource = 'original_media' AND operation = 'create'
            "#,
            [],
            |row| row.get(0),
        )
        .expect("remaining count query should work");
    assert_eq!(remaining_count, 1, "exactly one outbox row should remain");
    let remaining_ids: String = conn
        .query_row(
            r#"
            SELECT group_concat(object_id, ',')
            FROM sync_outbox
            WHERE resource = 'original_media' AND operation = 'create'
            "#,
            [],
            |row| row.get(0),
        )
        .expect("remaining ids query should work");
    assert_eq!(remaining_ids, "j10:e10:m1");
}
