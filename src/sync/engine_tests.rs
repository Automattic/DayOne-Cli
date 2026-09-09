use super::*;
use crate::models::{Entry, EntryId, JournalId, Moment, MomentId};
use crate::store::OutboxItem;
use crate::store::sqlite::EntryAttachmentRow;
use crate::store::traits::{FakeOutboxStatus, FakeOutboxStore, OutboxStore};
use crate::sync::phases::outbox::{drain_sync_outbox, process_outbox_item};
use anyhow::anyhow;
use serde_json::{Value, json};
use std::collections::HashMap;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[test]
fn pending_metadata_inspection_resets_only_journals_and_keeps_retry_progress() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let store = crate::store::sqlite::Store::open_at(temp.path().join("dayone.db"))
        .expect("store should open");
    store
        .set_sync_cursor("journals", Some("historical-cursor"))
        .expect("journal cursor should save");
    store
        .set_sync_cursor("entries:journal-1", Some("entry-cursor"))
        .expect("entry cursor should save");

    prepare_sync_cursors(&store, false).expect("cursor preparation should succeed");

    assert_eq!(
        store
            .get_sync_cursor("journals")
            .expect("journal cursor should load"),
        None
    );
    assert_eq!(
        store
            .get_sync_cursor("entries:journal-1")
            .expect("entry cursor should load")
            .as_deref(),
        Some("entry-cursor")
    );
    assert!(
        store
            .journal_metadata_inspection_is_pending()
            .expect("inspection state should load")
    );

    store
        .set_sync_cursor("journals", Some("page-one-cursor"))
        .expect("partial inspection cursor should save");
    prepare_sync_cursors(&store, false).expect("retry preparation should succeed");
    assert_eq!(
        store
            .get_sync_cursor("journals")
            .expect("journal cursor should load")
            .as_deref(),
        Some("page-one-cursor"),
        "a retry must resume the incomplete inspection"
    );

    store
        .complete_journal_metadata_inspection()
        .expect("completed inspection should clear the state");
    store
        .set_sync_cursor("journals", Some("post-backfill-cursor"))
        .expect("new journal cursor should save");
    prepare_sync_cursors(&store, false).expect("completed preparation should succeed");
    assert_eq!(
        store
            .get_sync_cursor("journals")
            .expect("journal cursor should load")
            .as_deref(),
        Some("post-backfill-cursor")
    );
}

#[test]
fn feature_flag_handles_array_shape() {
    let flags = json!([{ "name": "enable_daily_chat", "enabled": true }]);
    assert!(feature_enabled(&flags, &["enable_daily_chat"]));
}

#[test]
fn feature_flags_struct_resolves_from_array_payload() {
    let payload = json!([
        { "name": "enable_daily_chat", "enabled": true },
        { "name": "enable_daily_chat_memories", "enabled": true },
        { "name": "journal_collections", "enabled": false }
    ]);
    let flags = FeatureFlags::from_payload(&payload);
    assert!(flags.daily_chat);
    assert!(flags.context_library);
    assert!(!flags.journal_hierarchy);
}

#[test]
fn feature_flags_struct_resolves_from_object_payload() {
    let payload = json!({
        "daily_chat": true,
        "context_library": false,
        "journal_hierarchy": true
    });
    let flags = FeatureFlags::from_payload(&payload);
    assert!(flags.daily_chat);
    assert!(!flags.context_library);
    assert!(flags.journal_hierarchy);
}

#[test]
fn extract_items_supports_items_and_data() {
    let items_shape = json!({ "items": [{ "id": "a" }] });
    let data_shape = json!({ "data": [{ "id": "b" }] });
    assert_eq!(extract_items(&items_shape).len(), 1);
    assert_eq!(extract_items(&data_shape).len(), 1);
}

#[test]
fn builds_entry_envelope_moments_with_expected_shape() {
    let source = vec![Moment {
        id: Some(MomentId::from("4aace0cb954f415f96a599d1a6ffe114")),
        kind: Some("image".to_owned()),
        content_type: Some("image/jpeg".to_owned()),
        md5: Some("c4ca4238a0b923820dcc509a6f75849b".to_owned()),
        file_size: Some(999),
        height: Some(1280),
        width: Some(1280),
        pdf_name: None,
        thumbnail: Some(crate::models::moment::MomentThumbnail {
            content_type: Some("image/jpeg".to_owned()),
            md5: Some("9f94a3af61dd7f40385dec8726015a4d".to_owned()),
            height: Some(640),
            width: Some(640),
            file_size: Some(65700),
            extra: HashMap::new(),
        }),
        extra: HashMap::new(),
    }];
    let mapped = build_entry_envelope_moments(&source);
    let first = mapped
        .as_array()
        .and_then(|arr| arr.first())
        .expect("mapped envelope moments should contain one item");
    assert_eq!(
        first.get("id").and_then(Value::as_str),
        Some("4aace0cb954f415f96a599d1a6ffe114")
    );
    assert_eq!(
        first.get("momentType").and_then(Value::as_str),
        Some("image")
    );
    assert_eq!(
        first.get("contentType").and_then(Value::as_str),
        Some("image/jpeg")
    );
    assert_eq!(
        first.get("md5").and_then(Value::as_str),
        Some("c4ca4238a0b923820dcc509a6f75849b")
    );
    assert_eq!(first.get("fileSize").and_then(Value::as_i64), Some(999));
    let thumbnail = first
        .get("thumbnail")
        .and_then(Value::as_object)
        .expect("thumbnail should be mapped");
    assert_eq!(
        thumbnail.get("contentType").and_then(Value::as_str),
        Some("image/jpeg")
    );
    assert_eq!(
        thumbnail.get("md5").and_then(Value::as_str),
        Some("9f94a3af61dd7f40385dec8726015a4d")
    );
    assert_eq!(
        thumbnail.get("fileSize").and_then(Value::as_i64),
        Some(65700)
    );
}

#[test]
fn build_entry_content_from_local_preserves_deleted_at_for_tombstones() {
    let local = Entry {
        id: EntryId::from("entry-1"),
        journal_id: Some(JournalId::from("journal-1")),
        updated_at: None,
        deleted_at: Some("2026-03-27T15:00:00.000Z".to_owned()),
        cursor: None,
        date: None,
        user_edit_date: None,
        body: None,
        rich_text_json: None,
        payload: Some(json!({
            "id": "entry-1",
            "body": "hello"
        })),
        moments: Vec::new(),
        extra: HashMap::new(),
    };
    let local_value = serde_json::to_value(&local).expect("entry should serialize");
    let content = build_entry_content_from_local(&local_value, None, "entry-1");
    assert_eq!(content.get("id").and_then(Value::as_str), Some("entry-1"));
    assert_eq!(
        content.get("deleted_at").and_then(Value::as_str),
        Some("2026-03-27T15:00:00.000Z")
    );
}

#[test]
fn build_entry_content_from_local_preserves_deleted_at_for_root_object_tombstones() {
    let local = json!({
        "id": "entry-2",
        "journal_id": "journal-1",
        "deleted_at": "2026-03-27T16:00:00.000Z",
        "body": "hello-from-root"
    });
    let content = build_entry_content_from_local(&local, None, "entry-2");
    assert_eq!(content.get("id").and_then(Value::as_str), Some("entry-2"));
    assert_eq!(
        content.get("deleted_at").and_then(Value::as_str),
        Some("2026-03-27T16:00:00.000Z")
    );
}

#[test]
fn build_entry_content_from_local_overlays_known_fields_on_extra_fields() {
    let local = json!({
        "id": "entry-3",
        "body": "fresh body",
        "date": 456.0,
        "payload": {
            "body": "stale payload body"
        }
    });
    let content = build_entry_content_from_local(
        &local,
        Some(r#"{"futureField":"kept","body":"stale extra field"}"#),
        "entry-3",
    );
    assert_eq!(
        content.get("body").and_then(Value::as_str),
        Some("fresh body")
    );
    assert_eq!(content.get("date").and_then(Value::as_f64), Some(456.0));
    assert_eq!(
        content.get("futureField").and_then(Value::as_str),
        Some("kept")
    );
}

#[test]
fn build_entry_thumbnail_parts_includes_only_rows_with_bytes() {
    let rows = vec![
        EntryAttachmentRow {
            journal_id: "j1".to_owned(),
            entry_id: "e1".to_owned(),
            moment_id: "m1".to_owned(),
            file_path: "/tmp/a".to_owned(),
            moment_type: "image".to_owned(),
            content_type: "image/jpeg".to_owned(),
            file_size_bytes: 1,
            md5_body: "abc".to_owned(),
            width: None,
            height: None,
            duration_seconds: None,
            thumbnail_content_type: Some("image/jpeg".to_owned()),
            thumbnail_md5: Some("def".to_owned()),
            thumbnail_width: None,
            thumbnail_height: None,
            thumbnail_file_size_bytes: None,
            thumbnail_bytes: Some(vec![1, 2, 3]),
            entry_associated: false,
            original_uploaded: false,
            last_error: None,
        },
        EntryAttachmentRow {
            journal_id: "j1".to_owned(),
            entry_id: "e1".to_owned(),
            moment_id: "m2".to_owned(),
            file_path: "/tmp/b".to_owned(),
            moment_type: "image".to_owned(),
            content_type: "image/jpeg".to_owned(),
            file_size_bytes: 1,
            md5_body: "ghi".to_owned(),
            width: None,
            height: None,
            duration_seconds: None,
            thumbnail_content_type: None,
            thumbnail_md5: None,
            thumbnail_width: None,
            thumbnail_height: None,
            thumbnail_file_size_bytes: None,
            thumbnail_bytes: None,
            entry_associated: false,
            original_uploaded: false,
            last_error: None,
        },
    ];
    let parts = build_entry_thumbnail_parts(&rows);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].name, "thumbnail.m1");
    assert_eq!(parts[0].content_type, "image/jpeg");
    assert_eq!(parts[0].bytes, vec![1, 2, 3]);
}

#[test]
fn retry_delay_is_bounded_and_exponential() {
    // Base delays double per attempt: 1s, 1s, 2s, 4s, ... Jitter stays
    // within +/-25% of the base for every attempt and key.
    let keys = ["entry:j1:e1", "entry:j1:e2", "comment:j2:e3:c4", ""];
    for (attempt, base) in [(0, 1_000), (1, 1_000), (2, 2_000), (3, 4_000)] {
        for key in keys {
            let delay = retry_delay_ms(attempt, key);
            let min = base - base / 4;
            let max = base + base / 4;
            assert!(
                (min..=max).contains(&delay),
                "attempt {attempt} key {key:?}: delay {delay} outside [{min}, {max}]"
            );
        }
    }
    // Attempts are capped, so overflowing attempt counts reuse the max delay.
    assert_eq!(
        retry_delay_ms(OUTBOX_MAX_ATTEMPTS + 10, "entry:j1:e1"),
        retry_delay_ms(OUTBOX_MAX_ATTEMPTS, "entry:j1:e1")
    );
}

#[test]
fn retry_delay_jitter_is_deterministic_for_same_inputs() {
    for attempt in 1..=OUTBOX_MAX_ATTEMPTS {
        assert_eq!(
            retry_delay_ms(attempt, "entry:j1:e1"),
            retry_delay_ms(attempt, "entry:j1:e1"),
            "same item id and attempt must produce the same delay"
        );
    }
}

#[test]
fn outbox_retry_logic_works_with_fake_store_no_sqlite() {
    let fake = FakeOutboxStore::default();
    fake.enqueue_outbox_item("entry:j1:e1", "entry", "update", "j1:e1", "{}", 0)
        .expect("enqueue should work");
    let leased = fake
        .lease_outbox_items(i64::MAX, 1)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);
    let item = leased.first().cloned().expect("leased item should exist");
    let before = now_epoch_ms();
    mark_retry_or_failed(&fake, &item, "transient network error").expect("retry mark should work");
    let after = now_epoch_ms();

    let state = fake
        .state_for_id(&item.id)
        .expect("fake state should exist");
    assert_eq!(state.status, FakeOutboxStatus::Pending);
    assert_eq!(state.attempt_count, 1);
    assert_eq!(state.last_error.as_deref(), Some("transient network error"));
    let min_expected = before.saturating_add(retry_delay_ms(1, &item.id));
    let max_expected = after.saturating_add(retry_delay_ms(1, &item.id));
    assert!(
        (min_expected..=max_expected).contains(&state.next_attempt_epoch_ms),
        "next attempt should be scheduled with retry backoff"
    );
}

#[tokio::test]
async fn drain_outbox_defers_e2e_entry_unavailable_key_material_without_incrementing_attempts() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-e2e-key-material-defer-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Locked","encryption":{"vault":{}}}"#,
        )
        .expect("E2E journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"entry-1","body":"unsynced secret"}"#,
        )
        .expect("entry should save");
    store
        .enqueue_outbox_item(
            "entry:journal-1:entry-1",
            "entry",
            "update",
            "journal-1:entry-1",
            r#"{"journal_id":"journal-1","entry_id":"entry-1","queued_at_epoch_ms":1000}"#,
            0,
        )
        .expect("outbox item should save");
    let conn = store.connect().expect("connect should work");
    conn.execute(
        "UPDATE sync_outbox SET attempt_count = ?1 WHERE id = 'entry:journal-1:entry-1'",
        [OUTBOX_MAX_ATTEMPTS - 1],
    )
    .expect("seed high attempt count should work");

    let before = now_epoch_ms();
    let api = FakeApiClient::new();
    let mut resources = Vec::new();
    drain_sync_outbox(&store, &api, 0, profile.id, &mut resources, "outbox:post")
        .await
        .expect("unavailable E2E key material should defer, not fail the drain");
    let after = now_epoch_ms();

    let (status, attempt_count, next_attempt, last_error): (String, i64, i64, Option<String>) = conn
        .query_row(
            "SELECT status, attempt_count, next_attempt_epoch_ms, last_error FROM sync_outbox WHERE id = 'entry:journal-1:entry-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("outbox row should remain queued");

    assert_eq!(status, "pending");
    assert_eq!(
        attempt_count,
        OUTBOX_MAX_ATTEMPTS - 1,
        "E2E key-material deferral must not burn the final retry attempt"
    );
    assert!(
        (before + OUTBOX_DEFER_DELAY_MS..=after + OUTBOX_DEFER_DELAY_MS).contains(&next_attempt),
        "deferred row should be scheduled with the key-availability delay"
    );
    assert!(
        last_error
            .as_deref()
            .unwrap_or_default()
            .contains("E2E key material unavailable: profile encryption key is not configured"),
        "last_error should explain that local key material is unavailable"
    );
    assert_eq!(
        resources.last().map(|resource| resource.status.as_str()),
        Some("deferred")
    );

    conn.execute_batch(
        "UPDATE sync_outbox SET next_attempt_epoch_ms = 0;
         CREATE TRIGGER fail_defer_persistence
         BEFORE UPDATE ON sync_outbox
         WHEN NEW.status = 'pending'
              AND NEW.next_attempt_epoch_ms > OLD.next_attempt_epoch_ms
         BEGIN
           SELECT RAISE(ABORT, 'forced defer persistence failure');
         END;",
    )
    .expect("defer failure trigger should install");
    resources.clear();
    let error = drain_sync_outbox(&store, &api, 0, profile.id, &mut resources, "outbox:post")
        .await
        .expect_err("failed defer persistence should fail the drain");
    assert!(error.to_string().contains("failed to defer outbox item"));
    assert_eq!(
        resources.last().map(|resource| resource.status.as_str()),
        Some("error")
    );
}

#[tokio::test]
async fn drain_outbox_defers_e2e_comment_unavailable_key_material_without_http_call() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-e2e-comment-key-material-defer-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Locked","encryption":{"vault":{}}}"#,
        )
        .expect("E2E journal should save");
    store
        .enqueue_outbox_item(
            "comment:journal-1:entry-1:comment-1",
            "comment",
            "create",
            "journal-1:entry-1:comment-1",
            r#"{"id":"comment-1","journal_id":"journal-1","entry_id":"entry-1","content":"unsynced secret comment"}"#,
            0,
        )
        .expect("comment outbox item should save");
    let conn = store.connect().expect("connect should work");
    conn.execute(
        "UPDATE sync_outbox SET attempt_count = ?1 WHERE id = 'comment:journal-1:entry-1:comment-1'",
        [OUTBOX_MAX_ATTEMPTS - 1],
    )
    .expect("seed high attempt count should work");

    let before = now_epoch_ms();
    let api = FakeApiClient::new();
    let mut resources = Vec::new();
    drain_sync_outbox(&store, &api, 0, profile.id, &mut resources, "outbox:post")
        .await
        .expect("unavailable E2E key material should defer comment outbox work");
    let after = now_epoch_ms();

    let (status, attempt_count, next_attempt, last_error): (String, i64, i64, Option<String>) = conn
        .query_row(
            "SELECT status, attempt_count, next_attempt_epoch_ms, last_error FROM sync_outbox WHERE id = 'comment:journal-1:entry-1:comment-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("comment outbox row should remain queued");

    assert_eq!(status, "pending");
    assert_eq!(
        attempt_count,
        OUTBOX_MAX_ATTEMPTS - 1,
        "E2E comment key-material deferral must not burn the final retry attempt"
    );
    assert!(
        (before + OUTBOX_DEFER_DELAY_MS..=after + OUTBOX_DEFER_DELAY_MS).contains(&next_attempt),
        "deferred row should be scheduled with the key-availability delay"
    );
    assert!(
        last_error
            .as_deref()
            .unwrap_or_default()
            .contains("E2E key material unavailable: profile encryption key is not configured"),
        "last_error should explain that local key material is unavailable"
    );
    assert_eq!(
        resources.last().map(|resource| resource.status.as_str()),
        Some("deferred")
    );
    assert!(
        api.captured_post_json_bodies("/journals/journal-1/entries/entry-1/comments")
            .await
            .is_empty(),
        "comment create should not reach the API when E2E key material is unavailable"
    );
}

#[test]
fn build_journals_sync_params_only_excludes_deleted_for_empty_initial_store() {
    assert_eq!(
        build_journals_sync_params(None, 0),
        vec![("excludeDeleted".to_owned(), "true".to_owned())]
    );
    assert_eq!(
        build_journals_sync_params(Some(String::new()), 0),
        vec![("excludeDeleted".to_owned(), "true".to_owned())]
    );
    assert!(build_journals_sync_params(None, 1).is_empty());
    assert!(build_journals_sync_params(Some(String::new()), 1).is_empty());
    assert!(build_journals_sync_params(Some("cursor-123".to_owned()), 0).is_empty());
}

#[test]
fn outbox_defer_detection_requires_missing_keys_error_type() {
    let daily_chat_item = OutboxItem {
        id: "daily_chat_feed:update:2026-03-13".to_owned(),
        resource: "daily_chat_feed".to_owned(),
        operation: "update".to_owned(),
        object_id: "2026-03-13".to_owned(),
        payload_json: "{}".to_owned(),
        attempt_count: 0,
    };
    let context_item = OutboxItem {
        id: "context_item:memory-1".to_owned(),
        resource: "context_item".to_owned(),
        operation: "update".to_owned(),
        object_id: "memory-1".to_owned(),
        payload_json: "{}".to_owned(),
        attempt_count: 0,
    };
    let err = OutboxProcessError::Retryable(anyhow!("not a typed missing-key error"));
    assert!(!should_defer_outbox_item_until_keys(&daily_chat_item, &err));
    assert!(!should_defer_outbox_item_until_keys(&context_item, &err));
}

#[test]
fn outbox_defer_detection_includes_context_items_for_missing_key_errors() {
    let context_item = OutboxItem {
        id: "context_item:memory-1".to_owned(),
        resource: "context_item".to_owned(),
        operation: "update".to_owned(),
        object_id: "memory-1".to_owned(),
        payload_json: "{}".to_owned(),
        attempt_count: 0,
    };
    let err = crate::sync::crypto::encrypt_context_library_payload(
        json!({"id":"memory-1","content":"hello"}),
        &crate::sync::crypto::CryptoContext::default(),
    )
    .expect_err("missing active key should fail");
    let outbox_err = OutboxProcessError::Retryable(err);
    assert!(should_defer_outbox_item_until_keys(
        &context_item,
        &outbox_err
    ));
}

#[test]
fn outbox_defer_detection_includes_e2e_resources_for_unavailable_key_material() {
    let err = crate::sync::crypto::encrypt_entry_content_for_journal(
        &json!({"id":"journal-1","encryption":{"vault":{}}}),
        &json!({"id":"entry-1","body":"hello"}),
        None,
        Some("{}"),
    )
    .expect_err("unavailable E2E key material should fail before encryption");
    let outbox_err = OutboxProcessError::Retryable(err);

    for (resource, operation) in [
        ("entry", "update"),
        ("entry", "delete"),
        ("original_media", "create"),
        ("journal", "create"),
        ("journal", "update"),
        ("comment", "create"),
        ("comment", "update"),
    ] {
        let item = OutboxItem {
            id: format!("{resource}:{operation}:object-1"),
            resource: resource.to_owned(),
            operation: operation.to_owned(),
            object_id: "object-1".to_owned(),
            payload_json: "{}".to_owned(),
            attempt_count: OUTBOX_MAX_ATTEMPTS - 1,
        };
        assert!(
            should_defer_outbox_item_until_keys(&item, &outbox_err),
            "{resource}/{operation} should defer rather than dead-letter when E2E key material is unavailable"
        );
    }
}

#[test]
fn parse_entry_put_response_bytes_handles_json_only() {
    let bytes = br#"{"updated_at":"2026-03-13T12:00:00.000Z"}"#;
    let (record, payload) = parse_entry_put_response_bytes(bytes).expect("parse should succeed");
    assert_eq!(
        record.get("updated_at").and_then(Value::as_str),
        Some("2026-03-13T12:00:00.000Z")
    );
    assert!(payload.is_none());
}

#[test]
fn parse_entry_put_response_bytes_handles_json_with_payload_bytes() {
    let record = json!({"contentLength": 4, "updated_at": "2026-03-13T12:00:00.000Z"}).to_string();
    let mut bytes = record.into_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(b"ABCD");

    let (parsed, payload) = parse_entry_put_response_bytes(&bytes).expect("parse should succeed");
    assert_eq!(parsed.get("contentLength").and_then(Value::as_u64), Some(4));
    assert_eq!(payload, Some(b"ABCD".to_vec()));
}

#[test]
fn parse_entry_put_response_bytes_rejects_truncated_payload() {
    let record = json!({"contentLength": 5}).to_string();
    let mut bytes = record.into_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(b"ABCD");

    let err = parse_entry_put_response_bytes(&bytes).expect_err("should reject truncated payload");
    assert!(err.to_string().contains("truncated"));
}

#[test]
fn parse_entry_put_response_bytes_rejects_payload_length_overflow() {
    let record = json!({"contentLength": u64::MAX}).to_string();
    let mut bytes = record.into_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(b"X");

    let err = parse_entry_put_response_bytes(&bytes).expect_err("should reject overflow");
    let msg = err.to_string();
    assert!(msg.contains("exceeds usize") || msg.contains("length overflow"));
}

#[test]
fn resolve_outbox_edit_date_prefers_queued_epoch_ms() {
    let payload = EntryOutboxPayload {
        journal_id: "j1".to_owned(),
        entry_id: "e1".to_owned(),
        queued_at: Some("2026-03-13T10:00:00.000Z".to_owned()),
        queued_at_epoch_ms: Some(1_234_567_i64),
        edit_date_epoch_ms: None,
        entry_json: None,
    };
    let entry = Entry {
        id: EntryId::from("e1"),
        journal_id: Some(JournalId::from("j1")),
        updated_at: None,
        deleted_at: None,
        cursor: None,
        date: None,
        user_edit_date: Some(Value::String("2026-03-13T09:00:00.000Z".to_owned())),
        body: None,
        rich_text_json: None,
        payload: None,
        moments: Vec::new(),
        extra: HashMap::new(),
    };
    assert_eq!(
        resolve_outbox_edit_date_epoch_ms(&payload, &entry, None),
        1_234_567_i64
    );
}

#[test]
fn resolve_outbox_edit_date_parses_rfc3339_fallback() {
    let payload = EntryOutboxPayload {
        journal_id: "j1".to_owned(),
        entry_id: "e1".to_owned(),
        queued_at: Some("2026-03-13T10:00:00.000Z".to_owned()),
        queued_at_epoch_ms: None,
        edit_date_epoch_ms: None,
        entry_json: None,
    };
    let entry = Entry {
        id: EntryId::from("e1"),
        journal_id: Some(JournalId::from("j1")),
        updated_at: None,
        deleted_at: None,
        cursor: None,
        date: None,
        user_edit_date: None,
        body: None,
        rich_text_json: None,
        payload: None,
        moments: Vec::new(),
        extra: HashMap::new(),
    };
    let expected = OffsetDateTime::parse("2026-03-13T10:00:00.000Z", &Rfc3339)
        .expect("test timestamp should parse")
        .unix_timestamp_nanos() as i64
        / 1_000_000;
    assert_eq!(
        resolve_outbox_edit_date_epoch_ms(&payload, &entry, None),
        expected
    );
}

#[test]
fn resolve_outbox_edit_date_prefers_persisted_user_edit_date() {
    let payload = EntryOutboxPayload {
        journal_id: "j1".to_owned(),
        entry_id: "e1".to_owned(),
        queued_at: None,
        queued_at_epoch_ms: Some(1_234_567_i64),
        edit_date_epoch_ms: None,
        entry_json: None,
    };
    let entry = Entry {
        id: EntryId::from("e1"),
        journal_id: Some(JournalId::from("j1")),
        updated_at: None,
        deleted_at: None,
        cursor: None,
        date: None,
        user_edit_date: None,
        body: None,
        rich_text_json: None,
        payload: None,
        moments: Vec::new(),
        extra: HashMap::new(),
    };
    let expected = OffsetDateTime::parse("2026-03-13T08:00:00.000Z", &Rfc3339)
        .expect("test timestamp should parse")
        .unix_timestamp_nanos() as i64
        / 1_000_000;
    assert_eq!(
        resolve_outbox_edit_date_epoch_ms(&payload, &entry, Some("2026-03-13T08:00:00.000Z")),
        expected
    );
}

#[test]
fn entry_outbox_payload_supports_snake_case_keys() {
    let payload_json = json!({
        "journal_id": "j1",
        "entry_id": "e1",
        "queued_at": "2026-03-13T10:00:00.000Z",
        "queued_at_epoch_ms": 123_i64,
        "edit_date_epoch_ms": 456_i64
    });
    let parsed: EntryOutboxPayload =
        serde_json::from_value(payload_json).expect("snake_case outbox payload should parse");
    assert_eq!(parsed.journal_id, "j1");
    assert_eq!(parsed.entry_id, "e1");
    assert_eq!(
        parsed.queued_at.as_deref(),
        Some("2026-03-13T10:00:00.000Z")
    );
    assert_eq!(parsed.queued_at_epoch_ms, Some(123_i64));
    assert_eq!(parsed.edit_date_epoch_ms, Some(456_i64));
    assert!(parsed.entry_json.is_none());
}

#[test]
fn original_media_outbox_payload_supports_snake_case_keys() {
    let payload_json = json!({
        "journal_id": "j1",
        "entry_id": "e1",
        "moment_id": "m1"
    });
    let parsed: OriginalMediaOutboxPayload =
        serde_json::from_value(payload_json).expect("snake_case media outbox payload should parse");
    assert_eq!(parsed.journal_id, "j1");
    assert_eq!(parsed.entry_id, "e1");
    assert_eq!(parsed.moment_id, "m1");
}

#[test]
fn deleted_journal_entry_feed_errors_are_treated_as_non_fatal() {
    let err = anyhow!(crate::http::ApiError::new(
        404,
        "Not Found",
        "Journal has been deleted."
    ));
    assert!(is_deleted_journal_entries_feed_error(&err));

    let wrapped = err.context("sync failed after retry 3");
    assert!(is_deleted_journal_entries_feed_error(&wrapped));

    let case_variant = anyhow!(crate::http::ApiError::new(
        404,
        "Not Found",
        "journal HAS been DELETED."
    ));
    assert!(is_deleted_journal_entries_feed_error(&case_variant));

    let other_404 = anyhow!(crate::http::ApiError::new(404, "Not Found", "missing"));
    assert!(!is_deleted_journal_entries_feed_error(&other_404));

    let wrong_status = anyhow!(crate::http::ApiError::new(
        500,
        "Server Error",
        "Journal has been deleted."
    ));
    assert!(!is_deleted_journal_entries_feed_error(&wrong_status));

    let plain_string = anyhow!("HTTP 404 Not Found: Journal has been deleted.");
    assert!(!is_deleted_journal_entries_feed_error(&plain_string));
}

#[test]
fn parse_entries_feed_bytes_normalizes_numeric_string_fields() {
    let line = br#"{"id":123,"journalId":456,"updated_at":1775044526571,"deleted_at":1774990365021}
"#;
    let records = parse_entries_feed_bytes(line).expect("feed line should parse");
    let first = records.first().expect("record should exist");
    assert_eq!(first.record.id.as_ref(), "123");
    assert_eq!(
        first.record.journal_id.as_ref().map(|id| id.as_ref()),
        Some("456")
    );
    assert_eq!(first.record.updated_at.as_deref(), Some("1775044526571"));
    assert_eq!(first.record.deleted_at.as_deref(), Some("1774990365021"));
}

#[test]
fn decode_entry_outbox_payload_errors_are_non_retryable() {
    let item = OutboxItem {
        id: "item-1".to_owned(),
        resource: "entry".to_owned(),
        operation: "update".to_owned(),
        object_id: "e1".to_owned(),
        payload_json: r#"{"journal_id":"j1"}"#.to_owned(),
        attempt_count: 0,
    };
    let err = decode_entry_outbox_payload(&item).expect_err("decode should fail");
    assert!(matches!(err, OutboxProcessError::NonRetryable { .. }));
}

#[test]
fn decode_original_media_outbox_payload_errors_are_non_retryable() {
    let item = OutboxItem {
        id: "item-2".to_owned(),
        resource: "original_media".to_owned(),
        operation: "upload".to_owned(),
        object_id: "m1".to_owned(),
        payload_json: r#"{"journal_id":"j1","entry_id":"e1"}"#.to_owned(),
        attempt_count: 0,
    };
    let err = decode_original_media_outbox_payload(&item).expect_err("decode should fail");
    assert!(matches!(err, OutboxProcessError::NonRetryable { .. }));
}

#[test]
fn decode_context_item_outbox_payload_errors_are_non_retryable() {
    let item = OutboxItem {
        id: "item-3".to_owned(),
        resource: "context_item".to_owned(),
        operation: "update".to_owned(),
        object_id: "ctx1".to_owned(),
        payload_json: "{\"id\":".to_owned(),
        attempt_count: 0,
    };
    let err = decode_context_item_outbox_payload(&item).expect_err("decode should fail");
    assert!(matches!(err, OutboxProcessError::NonRetryable { .. }));
}

#[test]
fn decode_daily_chat_feed_outbox_payload_errors_are_non_retryable() {
    let item = OutboxItem {
        id: "item-4".to_owned(),
        resource: "daily_chat_feed".to_owned(),
        operation: "update".to_owned(),
        object_id: "2026-03-13".to_owned(),
        payload_json: "{\"date\":".to_owned(),
        attempt_count: 0,
    };
    let err = decode_daily_chat_feed_outbox_payload(&item).expect_err("decode should fail");
    assert!(matches!(err, OutboxProcessError::NonRetryable { .. }));
}

#[test]
fn decode_journal_create_outbox_payload_errors_are_non_retryable() {
    let item = OutboxItem {
        id: "item-5".to_owned(),
        resource: "journal".to_owned(),
        operation: "create".to_owned(),
        object_id: "j1".to_owned(),
        payload_json: "{\"name\":".to_owned(),
        attempt_count: 0,
    };
    let err = decode_journal_create_outbox_payload(&item).expect_err("decode should fail");
    assert!(matches!(err, OutboxProcessError::NonRetryable { .. }));
}

#[test]
fn merge_payload_value_keeps_known_fields_out_of_extra() {
    let mut record = Entry::default();
    let payload = json!({
        "id": "entry-1",
        "deleted_at": "2026-03-13T10:00:00.000Z",
        "richTextJSON": "{\"ops\":[]}",
        "user_edit_date": "2026-03-13T09:00:00.000Z",
        "unknown_payload_field": true
    });
    merge_payload_value(&mut record, payload);

    assert_eq!(record.id, EntryId::from("entry-1"));
    assert_eq!(
        record.deleted_at.as_deref(),
        Some("2026-03-13T10:00:00.000Z")
    );
    assert_eq!(
        record.rich_text_json.as_ref().and_then(Value::as_str),
        Some("{\"ops\":[]}")
    );
    assert_eq!(
        record.user_edit_date.as_ref().and_then(Value::as_str),
        Some("2026-03-13T09:00:00.000Z")
    );
    assert!(record.extra.contains_key("unknown_payload_field"));
    assert!(!record.extra.contains_key("id"));
    assert!(!record.extra.contains_key("deleted_at"));
    assert!(!record.extra.contains_key("richTextJSON"));
    assert!(!record.extra.contains_key("user_edit_date"));
}

#[tokio::test]
async fn push_entry_outbox_delete_sends_delete_envelope_without_update_only_fields() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dayone-cli-engine-delete-envelope-{unique}.db"));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Test Journal"}"#,
        )
        .expect("journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T01:00:00.000Z"),
            r#"{"id":"entry-1","body":"hello","deleted_at":"2026-03-17T01:00:00.000Z"}"#,
        )
        .expect("entry should save");

    let item = OutboxItem {
        id: "entry:journal-1:entry-1".to_owned(),
        resource: "entry".to_owned(),
        operation: "delete".to_owned(),
        object_id: "entry-1".to_owned(),
        payload_json:
            r#"{"journal_id":"journal-1","entry_id":"entry-1","queued_at_epoch_ms":1000}"#
                .to_owned(),
        attempt_count: 0,
    };

    let response_bytes = br#"{"updated_at":"2026-03-17T01:00:00.000Z"}"#.to_vec();
    let api = FakeApiClient::new()
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            response_bytes,
        )
        .await;

    push_entry_outbox(&store, &api, profile.id, &item)
        .await
        .expect("push should succeed");

    let envelope = api
        .last_put_entry_envelope()
        .await
        .expect("envelope should have been captured");

    assert_eq!(
        envelope.get("type").and_then(Value::as_str),
        Some("delete"),
        "delete outbox item should emit type=delete envelope"
    );
    assert_eq!(
        envelope.get("entryId").and_then(Value::as_str),
        Some("entry-1"),
        "envelope should include entryId"
    );
    assert!(
        envelope.get("entryDate").is_none(),
        "delete envelope must not include update-only field entryDate"
    );
    assert!(
        envelope.get("moments").is_none(),
        "delete envelope must not include update-only field moments"
    );
    assert!(
        envelope.get("editDate").is_some(),
        "delete envelope should include editDate"
    );
}

#[tokio::test]
async fn push_entry_outbox_drops_journal_not_found_when_local_journal_is_not_active() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-stale-journal-outbox-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-18T00:00:00.000Z"),
            r#"{"id":"journal-1","name":"Test Journal","state":"deleted"}"#,
        )
        .expect("deleted journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"entry-1","date":1000.0,"body":"stale local body"}"#,
        )
        .expect("entry should save");

    store
        .enqueue_outbox_item(
            "entry:journal-1:entry-1",
            "entry",
            "update",
            "journal-1:entry-1",
            r#"{"journal_id":"journal-1","entry_id":"entry-1","queued_at_epoch_ms":1000}"#,
            0,
        )
        .expect("outbox item should save");
    let api = FakeApiClient::new()
        .with_error_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            "Journal not found for user.",
        )
        .await;

    let mut resources = Vec::new();
    drain_sync_outbox(&store, &api, 0, profile.id, &mut resources, "outbox:post")
        .await
        .expect("stale outbox item should be dropped through the real drain path");

    let conn = store.connect().expect("connection should open");
    let outbox_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row.get(0))
        .expect("outbox count should query");
    assert_eq!(outbox_count, 0, "dropped outbox item should be removed");
    assert!(
        store
            .get_entry_json_row_with_extra_fields("entry-1")
            .expect("entry lookup should succeed")
            .is_some(),
        "dropping stale outbox should not delete local entry data"
    );
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].resource, "outbox:post");
    assert_eq!(resources[0].status, "success");
    assert_eq!(resources[0].changed_count, 1);
}

#[tokio::test]
async fn rejected_entry_update_fails_sync_and_retains_recovery_payload() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dayone-cli-entry-dirty-{unique}.db"));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            None,
            None,
            r#"{"id":"journal-1","name":"Test Journal"}"#,
        )
        .expect("journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("1712757600000"),
            Some("2024-04-10T12:00:00.000Z"),
            None,
            r#"{"id":"entry-1","date":1712757600000.0,"body":"local edit","moments":[],"tags":[]}"#,
        )
        .expect("entry should save");
    let payload = json!({
        "journal_id": "journal-1",
        "entry_id": "entry-1",
        "entry_json": {
            "id": "entry-1",
            "date": 1_712_757_600_000_f64,
            "body": "local edit",
            "moments": [],
            "tags": []
        },
        "queued_at": "2024-04-10T12:00:00.000Z",
        "queued_at_epoch_ms": 1_712_757_600_000_i64
    })
    .to_string();
    store
        .enqueue_outbox_item(
            "entry:journal-1:entry-1",
            "entry",
            "update",
            "journal-1:entry-1",
            &payload,
            0,
        )
        .expect("outbox item should enqueue");

    let api = FakeApiClient::new()
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            serde_json::to_vec(&json!({
                "outcome": "dirty",
                "revision": {
                    "entryId": "entry-1",
                    "type": "update",
                    "editDate": 1_712_757_650_000_i64
                }
            }))
            .expect("fixture should serialize"),
        )
        .await;

    let mut resources = Vec::new();
    let err = drain_sync_outbox(&store, &api, 1, profile.id, &mut resources, "outbox:post")
        .await
        .expect_err("rejected update must fail the sync");
    assert!(err.to_string().contains("outcome=dirty"));
    assert!(!err.to_string().contains("entry-1"));
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].status, "error");
    assert!(
        resources[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("outcome=dirty") && !error.contains("entry-1"))
    );

    let items = store
        .list_outbox_item_details(true)
        .expect("failed outbox should list");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, "failed");
    assert_eq!(items[0].payload_json.as_deref(), Some(payload.as_str()));
    let (local, _) = store
        .get_entry_json_row_with_extra_fields("entry-1")
        .expect("entry lookup should succeed")
        .expect("local entry should remain");
    assert!(local.contains("local edit"));
}

#[tokio::test]
async fn push_entry_outbox_keeps_journal_not_found_retryable_when_local_journal_is_active() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-active-journal-outbox-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Test Journal","state":"active"}"#,
        )
        .expect("active journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"entry-1","date":1000.0,"body":"local body"}"#,
        )
        .expect("entry should save");

    let item = OutboxItem {
        id: "entry:journal-1:entry-1".to_owned(),
        resource: "entry".to_owned(),
        operation: "update".to_owned(),
        object_id: "journal-1:entry-1".to_owned(),
        payload_json:
            r#"{"journal_id":"journal-1","entry_id":"entry-1","queued_at_epoch_ms":1000}"#
                .to_owned(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new()
        .with_error_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            "Journal not found for user.",
        )
        .await;

    let err = push_entry_outbox(&store, &api, profile.id, &item)
        .await
        .expect_err("active local journal should keep the outbox error retryable");
    assert!(
        err.to_string().contains("Journal not found for user"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn push_entry_outbox_prefers_payload_snapshot_when_local_row_was_clobbered() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dayone-cli-engine-entry-snapshot-{unique}.db"));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Test Journal"}"#,
        )
        .expect("journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r##"{"id":"entry-1","date":1000.0,"body":"old body","richTextJSON":"{\"contents\":[{\"text\":\"old body\"}],\"meta\":{\"version\":1}}","moments":[],"tags":[],"timeZone":null}"##,
        )
        .expect("stale local entry should save");

    let item = OutboxItem {
        id: "entry:journal-1:entry-1".to_owned(),
        resource: "entry".to_owned(),
        operation: "update".to_owned(),
        object_id: "journal-1:entry-1".to_owned(),
        payload_json: json!({
            "journal_id": "journal-1",
            "entry_id": "entry-1",
            "queued_at_epoch_ms": 1000_i64,
            "entry_json": {
                "id": "entry-1",
                "date": 2000.0,
                "body": "new body",
                "richTextJSON": "{\"contents\":[{\"text\":\"new body\"}],\"meta\":{\"version\":1,\"created\":{\"platform\":\"dayone-cli\"}}}",
                "moments": [],
                "tags": [],
                "timeZone": null
            }
        })
        .to_string(),
        attempt_count: 0,
    };

    let response_bytes = br#"{"updated_at":"2026-03-17T02:00:00.000Z"}"#.to_vec();
    let api = FakeApiClient::new()
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            response_bytes,
        )
        .await;

    push_entry_outbox(&store, &api, profile.id, &item)
        .await
        .expect("push should succeed");

    let (raw, _) = store
        .get_entry_json_row_with_extra_fields("entry-1")
        .expect("entry read should succeed")
        .expect("entry should exist");
    let value: Value = serde_json::from_str(&raw).expect("entry json should parse");
    assert_eq!(value.get("body").and_then(Value::as_str), Some("new body"));
    assert_eq!(
        value.get("richTextJSON").and_then(Value::as_str),
        Some(
            "{\"contents\":[{\"text\":\"new body\"}],\"meta\":{\"version\":1,\"created\":{\"platform\":\"dayone-cli\"}}}"
        )
    );
}

#[tokio::test]
async fn push_entry_outbox_tolerates_camel_and_snake_user_edit_date_in_snapshot() {
    // Conflict reconciliation used to inject a snake_case `user_edit_date` into
    // snapshots that already carried the payload's camelCase `userEditDate`.
    // Serde treats those as the same field (alias), so the push failed forever
    // with "duplicate field `user_edit_date`". The push must tolerate both keys.
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dayone-cli-engine-dup-alias-{unique}.db"));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Test Journal"}"#,
        )
        .expect("journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"entry-1","date":1000.0,"body":"moved body"}"#,
        )
        .expect("entry should save");

    let item = OutboxItem {
        id: "entry:journal-1:entry-1".to_owned(),
        resource: "entry".to_owned(),
        operation: "update".to_owned(),
        object_id: "journal-1:entry-1".to_owned(),
        payload_json: json!({
            "journal_id": "journal-1",
            "entry_id": "entry-1",
            "queued_at_epoch_ms": 1000_i64,
            "entry_json": {
                "id": "entry-1",
                "date": 1000.0,
                "body": "moved body",
                "userEditDate": 1_712_757_500_000_i64,
                "user_edit_date": "1712757600000",
                "moments": [],
                "tags": []
            }
        })
        .to_string(),
        attempt_count: 0,
    };

    let api = FakeApiClient::new()
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            br#"{"updated_at":"2026-03-17T02:00:00.000Z"}"#.to_vec(),
        )
        .await;

    push_entry_outbox(&store, &api, profile.id, &item)
        .await
        .expect("push should tolerate camelCase + snake_case user edit date keys");
}

#[tokio::test]
async fn push_entry_outbox_tolerates_null_moments_when_rebuilding_from_local_row() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dayone-cli-engine-null-moments-{unique}.db"));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Test Journal"}"#,
        )
        .expect("journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T01:00:00.000Z"),
            r#"{"id":"entry-1","body":"hello","moments":null,"deleted_at":"2026-03-17T01:00:00.000Z"}"#,
        )
        .expect("entry with null moments should save");

    let item = OutboxItem {
        id: "entry:journal-1:entry-1".to_owned(),
        resource: "entry".to_owned(),
        operation: "delete".to_owned(),
        object_id: "journal-1:entry-1".to_owned(),
        payload_json:
            r#"{"journal_id":"journal-1","entry_id":"entry-1","queued_at_epoch_ms":1000}"#
                .to_owned(),
        attempt_count: 0,
    };

    let api = FakeApiClient::new()
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            br#"{"updated_at":"2026-03-17T02:00:00.000Z"}"#.to_vec(),
        )
        .await;

    push_entry_outbox(&store, &api, profile.id, &item)
        .await
        .expect("push should tolerate a null moments key in local entry content");
}

#[tokio::test]
async fn push_entry_outbox_tolerates_lenient_moment_numeric_fields() {
    // Real client payloads have been observed with float or numeric-string
    // moment sizes; the push must not reject them.
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dayone-cli-engine-lenient-moment-{unique}.db"));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Test Journal"}"#,
        )
        .expect("journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"entry-1","body":"hello"}"#,
        )
        .expect("entry should save");

    let item = OutboxItem {
        id: "entry:journal-1:entry-1".to_owned(),
        resource: "entry".to_owned(),
        operation: "update".to_owned(),
        object_id: "journal-1:entry-1".to_owned(),
        payload_json: json!({
            "journal_id": "journal-1",
            "entry_id": "entry-1",
            "queued_at_epoch_ms": 1000_i64,
            "entry_json": {
                "id": "entry-1",
                "date": 1000.0,
                "body": "hello",
                "moments": [{
                    "id": "m1",
                    "type": "image",
                    "contentType": "image/jpeg",
                    "fileSize": 42.0,
                    "width": "640",
                    "height": 480
                }]
            }
        })
        .to_string(),
        attempt_count: 0,
    };

    let api = FakeApiClient::new()
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            br#"{"updated_at":"2026-03-17T02:00:00.000Z"}"#.to_vec(),
        )
        .await;

    push_entry_outbox(&store, &api, profile.id, &item)
        .await
        .expect("push should tolerate float and numeric-string moment fields");

    let envelope = api
        .last_put_entry_envelope()
        .await
        .expect("envelope should have been captured");
    let moment = envelope
        .get("moments")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .expect("envelope should carry the moment");
    assert_eq!(moment.get("fileSize").and_then(Value::as_i64), Some(42));
    assert_eq!(moment.get("width").and_then(Value::as_i64), Some(640));
}

#[tokio::test]
async fn push_entry_outbox_update_strips_stray_deleted_at_from_wire_and_local_row() {
    // A failed `entry delete` followed by `entry write` used to leave a stray
    // `deleted_at` key inside the update snapshot. Pushing it re-tombstoned the
    // local row and could delete the entry server-side. Updates must never
    // carry deletion markers.
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dayone-cli-engine-stray-tombstone-{unique}.db"));
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .expect("profile should create");
    store
        .upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Test Journal"}"#,
        )
        .expect("journal should save");
    store
        .upsert_entry_json_row_with_edit_date(
            "entry-1",
            "journal-1",
            Some("2026-03-17T00:00:00.000Z"),
            Some("2026-03-17T00:00:00.000Z"),
            None,
            r#"{"id":"entry-1","body":"rewritten body","deleted_at":"2026-03-17T01:00:00.000Z"}"#,
        )
        .expect("entry should save");

    let item = OutboxItem {
        id: "entry:journal-1:entry-1".to_owned(),
        resource: "entry".to_owned(),
        operation: "update".to_owned(),
        object_id: "journal-1:entry-1".to_owned(),
        payload_json: json!({
            "journal_id": "journal-1",
            "entry_id": "entry-1",
            "queued_at_epoch_ms": 1000_i64,
            "entry_json": {
                "id": "entry-1",
                "date": 1000.0,
                "body": "rewritten body",
                "deleted_at": "2026-03-17T01:00:00.000Z"
            }
        })
        .to_string(),
        attempt_count: 0,
    };

    let api = FakeApiClient::new()
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/journal-1/entry-1",
            br#"{"updated_at":"2026-03-17T02:00:00.000Z"}"#.to_vec(),
        )
        .await;

    push_entry_outbox(&store, &api, profile.id, &item)
        .await
        .expect("update push should succeed");

    let wire_content = api
        .last_put_entry_content_json()
        .await
        .expect("plaintext content part should be JSON");
    assert!(
        wire_content.get("deleted_at").is_none() && wire_content.get("deletedAt").is_none(),
        "update push must not send deletion markers, got: {wire_content}"
    );

    let (raw, _) = store
        .get_entry_json_row_with_extra_fields("entry-1")
        .expect("entry read should succeed")
        .expect("entry should exist");
    let value: Value = serde_json::from_str(&raw).expect("entry json should parse");
    assert!(
        value.get("deleted_at").is_none(),
        "update push must not persist deletion markers locally, got: {raw}"
    );
    let conn = store.connect().expect("connection should open");
    let (deleted_at, is_deleted): (Option<String>, i64) = conn
        .query_row(
            "SELECT deleted_at, is_deleted FROM entries WHERE id = 'entry-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("entry row should exist");
    assert_eq!(deleted_at, None, "tombstone column must stay cleared");
    assert_eq!(is_deleted, 0, "is_deleted must stay cleared");
}

#[tokio::test]
async fn process_outbox_item_comment_create_reconciles_local_id() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dayone-cli-engine-comment-create-{unique}.db"));
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "j1",
            Some("2026-04-01T00:00:00.000Z"),
            None,
            r#"{"id":"j1","name":"Journal 1"}"#,
        )
        .expect("journal should save");
    store
        .upsert_comment_json_row(
            &json!({
                "id": "local-c1",
                "journal_id": "j1",
                "entry_id": "e1",
                "content": "hello"
            })
            .to_string(),
        )
        .expect("local placeholder should save");

    let item = OutboxItem {
        id: "comment:create:j1:e1:local-c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "create".to_owned(),
        object_id: "j1:e1:local-c1".to_owned(),
        payload_json: json!({
            "id": "local-c1",
            "journal_id": "j1",
            "entry_id": "e1",
            "content": "hello"
        })
        .to_string(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new()
        .with_json_fixture(
            "POST_JSON /journals/j1/entries/e1/comments",
            json!({"id":"server-c1"}),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({
                "finished": true,
                "comments": [{
                    "id": "server-c1",
                    "content": "aGVsbG8="
                }]
            }),
        )
        .await;

    process_outbox_item(&store, &api, 0, &item)
        .await
        .expect("comment create should process");
    assert!(
        store
            .get_comment_json_row_by_id("local-c1")
            .expect("lookup should succeed")
            .is_none(),
        "local ID should be remapped away"
    );
    assert!(
        store
            .get_comment_json_row_by_id("server-c1")
            .expect("lookup should succeed")
            .is_some(),
        "server ID should exist after reconcile"
    );
}

#[tokio::test]
async fn process_outbox_item_comment_update_and_delete_use_expected_paths() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-comment-update-delete-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "j1",
            Some("2026-04-01T00:00:00.000Z"),
            None,
            r#"{"id":"j1","name":"Journal 1"}"#,
        )
        .expect("journal should save");

    let update_item = OutboxItem {
        id: "comment:update:j1:e1:c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "update".to_owned(),
        object_id: "j1:e1:c1".to_owned(),
        payload_json: json!({
            "id": "c1",
            "journal_id": "j1",
            "entry_id": "e1",
            "content": "updated"
        })
        .to_string(),
        attempt_count: 0,
    };
    let delete_item = OutboxItem {
        id: "comment:delete:j1:e1:c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "delete".to_owned(),
        object_id: "j1:e1:c1".to_owned(),
        payload_json: json!({
            "id": "c1",
            "journal_id": "j1",
            "entry_id": "e1"
        })
        .to_string(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new()
        .with_put_json_fixture(
            "PUT_JSON /journals/j1/entries/e1/comments/c1",
            json!({"ok": true}),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({"finished": true, "comments": []}),
        )
        .await
        .with_delete_json_fixture(
            "DELETE_JSON /journals/j1/entries/e1/comments/c1",
            json!(null),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({"finished": true, "comments": []}),
        )
        .await;

    process_outbox_item(&store, &api, 0, &update_item)
        .await
        .expect("comment update should process");
    process_outbox_item(&store, &api, 0, &delete_item)
        .await
        .expect("comment delete should process");
}

#[tokio::test]
async fn process_outbox_item_e2e_comment_delete_does_not_require_key_material() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-e2e-comment-delete-no-keys-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "j1",
            Some("2026-04-01T00:00:00.000Z"),
            None,
            r#"{"id":"j1","name":"Locked","encryption":{"vault":{}}}"#,
        )
        .expect("E2E journal should save");

    let item = OutboxItem {
        id: "comment:delete:j1:e1:c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "delete".to_owned(),
        object_id: "j1:e1:c1".to_owned(),
        payload_json: json!({
            "id": "c1",
            "journal_id": "j1",
            "entry_id": "e1"
        })
        .to_string(),
        attempt_count: OUTBOX_MAX_ATTEMPTS - 1,
    };
    let api = FakeApiClient::new()
        .with_delete_json_fixture(
            "DELETE_JSON /journals/j1/entries/e1/comments/c1",
            json!(null),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({"finished": true, "comments": []}),
        )
        .await;

    process_outbox_item(&store, &api, 0, &item)
        .await
        .expect("E2E comment delete should not require local key material");
}

#[tokio::test]
async fn process_outbox_item_comment_reconcile_skips_reactions_for_deleted_comments() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-comment-deleted-reactions-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "j1",
            Some("2026-04-01T00:00:00.000Z"),
            None,
            r#"{"id":"j1","name":"Journal 1"}"#,
        )
        .expect("journal should save");
    store
        .upsert_comment_reaction_json_row(
            &json!({
                "id": "r1",
                "journal_id": "j1",
                "entry_id": "e1",
                "comment_id": "c1",
                "user_id": "u1",
                "reaction": "like"
            })
            .to_string(),
        )
        .expect("seed reaction should save");

    let item = OutboxItem {
        id: "comment:delete:j1:e1:c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "delete".to_owned(),
        object_id: "j1:e1:c1".to_owned(),
        payload_json: json!({
            "id": "c1",
            "journal_id": "j1",
            "entry_id": "e1"
        })
        .to_string(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new()
        .with_delete_json_fixture(
            "DELETE_JSON /journals/j1/entries/e1/comments/c1",
            json!(null),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({
                "finished": true,
                "comments": [{
                    "id": "c1",
                    "deletedAt": "2026-04-21T12:00:00.000Z",
                    "reactions": [{
                        "id": "r2",
                        "user_id": "u2",
                        "reaction": "like"
                    }]
                }]
            }),
        )
        .await;

    process_outbox_item(&store, &api, 0, &item)
        .await
        .expect("comment delete should process");
    let rows = store
        .list_comment_reactions_json_rows_by_comment("j1", "e1", "c1")
        .expect("reaction lookup should succeed");
    assert!(
        rows.is_empty(),
        "deleted comments should not retain or reinsert reactions"
    );
}

#[tokio::test]
async fn process_outbox_item_comment_update_fails_when_comment_pagination_stalls() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-comment-stalled-page-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "j1",
            Some("2026-04-01T00:00:00.000Z"),
            None,
            r#"{"id":"j1","name":"Journal 1"}"#,
        )
        .expect("journal should save");
    let item = OutboxItem {
        id: "comment:update:j1:e1:c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "update".to_owned(),
        object_id: "j1:e1:c1".to_owned(),
        payload_json: json!({
            "id": "c1",
            "journal_id": "j1",
            "entry_id": "e1",
            "content": "updated"
        })
        .to_string(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new()
        .with_put_json_fixture(
            "PUT_JSON /journals/j1/entries/e1/comments/c1",
            json!({"ok": true}),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({"finished": false, "comments": []}),
        )
        .await;

    let err = process_outbox_item(&store, &api, 0, &item)
        .await
        .expect_err("stalled pagination should fail");
    let message = err.to_string();
    assert!(
        message.contains("pagination stalled"),
        "unexpected error: {message}"
    );
}

#[tokio::test]
async fn process_outbox_item_comment_create_missing_journal_is_non_retryable() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-comment-missing-journal-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    let item = OutboxItem {
        id: "comment:create:j1:e1:c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "create".to_owned(),
        object_id: "j1:e1:c1".to_owned(),
        payload_json: json!({
            "id": "c1",
            "journal_id": "j1",
            "entry_id": "e1",
            "content": "hello"
        })
        .to_string(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new();

    let err = process_outbox_item(&store, &api, 0, &item)
        .await
        .expect_err("missing journal should fail");
    let message = err.to_string();
    assert!(
        message.contains("missing locally for comment outbox push"),
        "unexpected error: {message}"
    );
}

#[tokio::test]
async fn process_outbox_item_comment_reconcile_does_not_advance_cursor_on_persist_error() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-comment-cursor-persist-error-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "j1",
            Some("2026-04-01T00:00:00.000Z"),
            None,
            r#"{"id":"j1","name":"Journal 1"}"#,
        )
        .expect("journal should save");
    let cursor_key = Store::comments_cursor_key("j1", "e1");
    store
        .set_sync_cursor(&cursor_key, Some("cursor-old"))
        .expect("seed cursor should save");

    let item = OutboxItem {
        id: "comment:update:j1:e1:c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "update".to_owned(),
        object_id: "j1:e1:c1".to_owned(),
        payload_json: json!({
            "id": "c1",
            "journal_id": "j1",
            "entry_id": "e1",
            "content": "updated"
        })
        .to_string(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new()
        .with_put_json_fixture(
            "PUT_JSON /journals/j1/entries/e1/comments/c1",
            json!({"ok": true}),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({
                "finished": false,
                "cursor": "cursor-new",
                "comments": [42]
            }),
        )
        .await;

    let err = process_outbox_item(&store, &api, 0, &item)
        .await
        .expect_err("invalid comment payload should fail persistence");
    assert!(
        err.to_string().contains("missing or empty id")
            || err.to_string().contains("expected a JSON object"),
        "unexpected error: {err}"
    );
    let cursor = store
        .get_sync_cursor(&cursor_key)
        .expect("cursor read should succeed");
    assert_eq!(
        cursor.as_deref(),
        Some("cursor-old"),
        "cursor should not advance on persist failure"
    );
}

#[tokio::test]
async fn process_outbox_item_comment_reconcile_keeps_existing_cursor_when_page_has_no_cursor() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-comment-cursor-preserve-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "j1",
            Some("2026-04-01T00:00:00.000Z"),
            None,
            r#"{"id":"j1","name":"Journal 1"}"#,
        )
        .expect("journal should save");
    let cursor_key = Store::comments_cursor_key("j1", "e1");
    store
        .set_sync_cursor(&cursor_key, Some("cursor-old"))
        .expect("seed cursor should save");

    let item = OutboxItem {
        id: "comment:update:j1:e1:c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "update".to_owned(),
        object_id: "j1:e1:c1".to_owned(),
        payload_json: json!({
            "id": "c1",
            "journal_id": "j1",
            "entry_id": "e1",
            "content": "updated"
        })
        .to_string(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new()
        .with_put_json_fixture(
            "PUT_JSON /journals/j1/entries/e1/comments/c1",
            json!({"ok": true}),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({
                "finished": true,
                "comments": []
            }),
        )
        .await;

    process_outbox_item(&store, &api, 0, &item)
        .await
        .expect("reconcile should succeed");
    let cursor = store
        .get_sync_cursor(&cursor_key)
        .expect("cursor read should succeed");
    assert_eq!(
        cursor.as_deref(),
        Some("cursor-old"),
        "missing cursor token on a finished page should not clear existing cursor"
    );
}

#[tokio::test]
async fn process_outbox_item_comment_create_failure_keeps_existing_cursor() {
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-engine-comment-create-cursor-preserve-{unique}.db"
    ));
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "j1",
            Some("2026-04-01T00:00:00.000Z"),
            None,
            r#"{"id":"j1","name":"Journal 1"}"#,
        )
        .expect("journal should save");
    let cursor_key = Store::comments_cursor_key("j1", "e1");
    store
        .set_sync_cursor(&cursor_key, Some("cursor-old"))
        .expect("seed cursor should save");

    let item = OutboxItem {
        id: "comment:create:j1:e1:local-c1".to_owned(),
        resource: "comment".to_owned(),
        operation: "create".to_owned(),
        object_id: "j1:e1:local-c1".to_owned(),
        payload_json: json!({
            "id": "local-c1",
            "journal_id": "j1",
            "entry_id": "e1",
            "content": "hello"
        })
        .to_string(),
        attempt_count: 0,
    };
    let api = FakeApiClient::new()
        .with_json_fixture(
            "POST_JSON /journals/j1/entries/e1/comments",
            json!({"id":"server-c1"}),
        )
        .await
        .with_json_fixture(
            "GET_JSON /journals/j1/entries/e1/comments",
            json!({
                "finished": false,
                "cursor": "cursor-new",
                "comments": [42]
            }),
        )
        .await;

    let err = process_outbox_item(&store, &api, 0, &item)
        .await
        .expect_err("invalid payload should fail persistence");
    assert!(
        err.to_string().contains("missing or empty id")
            || err.to_string().contains("expected a JSON object"),
        "unexpected error: {err}"
    );
    let cursor = store
        .get_sync_cursor(&cursor_key)
        .expect("cursor read should succeed");
    assert_eq!(
        cursor.as_deref(),
        Some("cursor-old"),
        "failed create reconcile should not clear existing cursor"
    );
}

async fn lock_check_fixture(
    journal: serde_json::Value,
    flags: serde_json::Value,
) -> (
    tempfile::TempDir,
    crate::store::sqlite::Store,
    i64,
    crate::http::fake::FakeApiClient,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::sqlite::Store::open_at(&dir.path().join("test.db")).unwrap();
    let profile = store
        .get_or_create_profile_for_base_url("https://stg.dayone.me")
        .unwrap();
    store
        .save_auth_session(
            profile.id,
            "synthetic-token",
            "2026-09-01T00:00:00Z",
            r#"{"id":"owner"}"#,
        )
        .unwrap();
    store
        .upsert_singleton_json_row("feature_flags", 1, &flags.to_string(), None)
        .unwrap();
    store
        .upsert_json_row("journals", "j", None, None, &journal.to_string())
        .unwrap();
    let entry = json!({"id":"e", "body":"retained text", "date":1712757600000_i64, "moments":[], "tags":[]});
    store
        .upsert_entry_json_row_with_edit_date(
            "e",
            "j",
            Some("1712757600000"),
            None,
            None,
            &entry.to_string(),
        )
        .unwrap();
    let payload = json!({"journal_id":"j","entry_id":"e","entry_json":entry,"queued_at_epoch_ms":1712757600000_i64}).to_string();
    store
        .enqueue_outbox_item("entry:j:e", "entry", "update", "j:e", &payload, 0)
        .unwrap();
    let api = crate::http::fake::FakeApiClient::new()
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/j/e",
            serde_json::to_vec(
                &json!({"outcome":"clean","revision":{"entryId":"e","type":"update"}}),
            )
            .unwrap(),
        )
        .await;
    (dir, store, profile.id, api)
}

fn relaxed_lock_journal() -> serde_json::Value {
    json!({"id":"j","is_shared":true,"shared_permissions":"any_participant_full"})
}
fn lock_response(user: &str, device: &str) -> serde_json::Value {
    json!({"lease":{"holder_user_id":user,"holder_device_id":device,"expires_at":"2026-09-01T00:02:00Z"},"server_time":"2026-09-01T00:00:00Z"})
}

// Advance the retry schedule without sleeping or changing the queued snapshot.
fn make_lock_retry_due(store: &crate::store::sqlite::Store) {
    store
        .connect()
        .unwrap()
        .execute(
            "UPDATE sync_outbox SET next_attempt_epoch_ms = 0 WHERE id = 'entry:j:e'",
            [],
        )
        .unwrap();
}

async fn assert_foreign_lease_waits(user: &str, device: &str) {
    let (_dir, store, profile, api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    let original = store.list_outbox_item_details(true).unwrap()[0]
        .payload_json
        .clone();
    let api = api
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            lock_response(user, device),
        )
        .await;
    let mut resources = vec![];
    let before = now_epoch_ms();
    let result = drain_sync_outbox(&store, &api, 1, profile, &mut resources, "outbox:post").await;
    assert!(api.last_put_entry_content_json().await.is_none());
    let item = &store.list_outbox_item_details(true).unwrap()[0];
    assert_eq!(
        item.status, "pending",
        "an active lease is temporary, not a failed upload"
    );
    assert_eq!(item.payload_json, original);
    assert_eq!(item.attempt_count, 0);
    assert!(
        item.next_attempt_epoch_ms > before,
        "do not spin on an active lease"
    );
    assert!(
        item.last_error
            .as_deref()
            .unwrap()
            .contains("entry_edit_locked")
    );
    result.expect("waiting on an editor should not fail the sync");
    assert_eq!(resources[0].status, "deferred");
    assert_eq!(resources[0].changed_count, 0);
    // There is no second response fixture: an immediate sync must respect the delay.
    drain_sync_outbox(&store, &api, 2, profile, &mut vec![], "outbox:post")
        .await
        .unwrap();
    assert_eq!(
        store.list_outbox_item_details(true).unwrap()[0].payload_json,
        original
    );
    assert_eq!(
        store.list_outbox_item_details(false).unwrap()[0].attempt_count,
        0
    );
    assert!(api.last_put_entry_content_json().await.is_none());
}

#[tokio::test]
async fn entry_lock_other_user_waits_without_uploading() {
    assert_foreign_lease_waits("other", "other-device").await;
}

#[tokio::test]
async fn entry_lock_same_user_other_device_waits_without_uploading() {
    assert_foreign_lease_waits("owner", "other-device").await;
}

#[tokio::test]
async fn entry_lock_release_automatically_uploads_the_original_snapshot() {
    let (_dir, store, profile, api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    let api = api
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            lock_response("other", "device"),
        )
        .await
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            json!({"lease":null,"server_time":"2026-09-01T00:03:00Z"}),
        )
        .await;
    let _ = drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post").await;
    assert!(api.last_put_entry_content_json().await.is_none());
    // A refreshed local row must not replace the saved edit or its timestamp.
    store
        .upsert_entry_json_row_with_edit_date(
            "e",
            "j",
            Some("2026-09-07T00:00:00Z"),
            None,
            None,
            r#"{"id":"e","body":"different local row"}"#,
        )
        .unwrap();
    make_lock_retry_due(&store);
    drain_sync_outbox(&store, &api, 2, profile, &mut vec![], "outbox:post")
        .await
        .unwrap();
    assert_eq!(
        api.last_put_entry_content_json()
            .await
            .expect("release should allow upload without manual retry")["body"],
        "retained text"
    );
    assert_eq!(
        api.last_put_entry_envelope().await.unwrap()["editDate"],
        1712757600000_f64
    );
    assert!(store.list_outbox_item_details(false).unwrap().is_empty());
}

#[tokio::test]
async fn entry_lock_wait_does_not_exhaust_upload_retries() {
    let (_dir, store, profile, mut api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    let original = store.list_outbox_item_details(true).unwrap()[0]
        .payload_json
        .clone();
    // Lock checks must preserve attempts already spent on real upload failures.
    store
        .connect()
        .unwrap()
        .execute(
            "UPDATE sync_outbox SET attempt_count = ?1 WHERE id = 'entry:j:e'",
            [OUTBOX_MAX_ATTEMPTS - 1],
        )
        .unwrap();
    for _ in 0..=OUTBOX_MAX_ATTEMPTS {
        api = api
            .with_json_fixture(
                "GET_JSON /shares/j/entries/e/lock",
                lock_response("other", "device"),
            )
            .await;
        make_lock_retry_due(&store);
        let _ = drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post").await;
        let item = &store.list_outbox_item_details(true).unwrap()[0];
        assert_eq!(item.status, "pending");
        assert_eq!(
            item.attempt_count,
            OUTBOX_MAX_ATTEMPTS - 1,
            "lock checks must not consume upload retries"
        );
        assert_eq!(item.payload_json, original);
    }
    assert!(api.last_put_entry_content_json().await.is_none());
    let api = api
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            json!({"lease":null,"server_time":"2026-09-01T00:03:00Z"}),
        )
        .await;
    make_lock_retry_due(&store);
    drain_sync_outbox(&store, &api, 2, profile, &mut vec![], "outbox:post")
        .await
        .unwrap();
    assert!(store.list_outbox_item_details(false).unwrap().is_empty());
}

#[tokio::test]
async fn entry_lock_wait_allows_later_uploads_and_reports_their_errors() {
    for (journal_id, outcome) in [("personal", "clean"), ("j", "clean"), ("personal", "dirty")] {
        let (_dir, store, profile, api) =
            lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
        if journal_id == "personal" {
            store.upsert_json_row("journals", journal_id, None, None,
                r#"{"id":"personal","is_shared":false,"shared_permissions":"any_participant_full"}"#).unwrap();
        }
        let payload = json!({"journal_id":journal_id,"entry_id":"other","entry_json":{"id":"other","body":"unrelated edit","date":1712757600000_i64,"moments":[],"tags":[]},"queued_at_epoch_ms":1712757600000_i64});
        store
            .enqueue_outbox_item(
                &format!("entry:{journal_id}:other"),
                "entry",
                "update",
                &format!("{journal_id}:other"),
                &payload.to_string(),
                1,
            )
            .unwrap();
        let mut api = api
            .with_json_fixture(
                "GET_JSON /shares/j/entries/e/lock",
                lock_response("other", "device"),
            )
            .await
            .with_bytes_fixture(
                format!("PUT_ENTRY_MULTIPART /v3/sync/entries/{journal_id}/other"),
                serde_json::to_vec(
                    &json!({"outcome":outcome,"revision":{"entryId":"other","type":"update"}}),
                )
                .unwrap(),
            )
            .await;
        if journal_id == "j" {
            api = api
                .with_json_fixture(
                    "GET_JSON /shares/j/entries/other/lock",
                    json!({"lease":null,"server_time":"2026-09-01T00:00:00Z"}),
                )
                .await;
        }
        let mut resources = vec![];
        let result =
            drain_sync_outbox(&store, &api, 1, profile, &mut resources, "outbox:post").await;
        assert_eq!(
            api.last_put_entry_content_json()
                .await
                .expect("a locked entry must not stop unrelated uploads")["body"],
            "unrelated edit"
        );
        if outcome == "dirty" {
            let err = result.expect_err("a deferred lock must not hide a later conflict");
            assert!(
                err.to_string().contains("outcome=dirty"),
                "wrong error: {err}"
            );
            assert_eq!(resources[0].status, "error");
            assert!(
                resources[0]
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("outcome=dirty")
            );
            let items = store.list_outbox_item_details(false).unwrap();
            assert_eq!(
                items.iter().find(|i| i.id == "entry:j:e").unwrap().status,
                "pending"
            );
            assert_eq!(
                items
                    .iter()
                    .find(|i| i.object_id == "personal:other")
                    .unwrap()
                    .status,
                "failed"
            );
            continue;
        }
        result.unwrap();
        let remaining = store.list_outbox_item_details(false).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "entry:j:e");
        assert_eq!(remaining[0].status, "pending");
        assert_eq!(resources[0].status, "deferred");
        assert_eq!(resources[0].changed_count, 1);
    }
}

#[tokio::test]
async fn entry_lock_release_then_server_conflict_requires_explicit_recovery() {
    let (_dir, store, profile, _) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    let original = store.list_outbox_item_details(true).unwrap()[0]
        .payload_json
        .clone();
    let api = crate::http::fake::FakeApiClient::new()
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            lock_response("other", "device"),
        )
        .await
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            json!({"lease":null,"server_time":"2026-09-01T00:03:00Z"}),
        )
        .await
        .with_bytes_fixture(
            "PUT_ENTRY_MULTIPART /v3/sync/entries/j/e",
            serde_json::to_vec(
                &json!({"outcome":"dirty","revision":{"entryId":"e","type":"update"}}),
            )
            .unwrap(),
        )
        .await;
    let _ = drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post").await;
    make_lock_retry_due(&store);
    let err = drain_sync_outbox(&store, &api, 2, profile, &mut vec![], "outbox:post")
        .await
        .expect_err("server conflict must require recovery");
    assert!(err.to_string().contains("outcome=dirty"));
    assert_eq!(
        api.last_put_entry_content_json().await.unwrap()["body"],
        "retained text"
    );
    let item = &store.list_outbox_item_details(true).unwrap()[0];
    assert_eq!(item.status, "failed");
    assert_eq!(item.payload_json, original);
    let unused_api = crate::http::fake::FakeApiClient::new();
    drain_sync_outbox(&store, &unused_api, 3, profile, &mut vec![], "outbox:post")
        .await
        .unwrap();
    assert!(unused_api.last_put_entry_content_json().await.is_none());
    assert_eq!(
        store.list_outbox_item_details(true).unwrap()[0].payload_json,
        original
    );
}

#[tokio::test]
async fn entry_lock_own_lease_allows_upload() {
    let (_dir, store, profile, api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    let device = crate::http::DeviceInfo::resolve(Some("owner"));
    let api = api
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            lock_response("owner", &device.id),
        )
        .await;
    drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
        .await
        .unwrap();
    assert_eq!(
        api.last_put_entry_content_json().await.unwrap()["body"],
        "retained text"
    );
}

#[tokio::test]
async fn entry_lock_absent_lease_allows_upload() {
    let (_dir, store, profile, api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    let api = api
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            json!({"lease":null,"server_time":"2026-09-01T00:00:00Z"}),
        )
        .await;
    drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
        .await
        .unwrap();
    assert!(store.list_outbox_item_details(false).unwrap().is_empty());
}

#[tokio::test]
async fn entry_lock_unavailable_status_is_retryable_and_does_not_upload() {
    let (_dir, store, profile, api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
        .await
        .expect_err("missing status response cannot permit upload");
    assert!(api.last_put_entry_content_json().await.is_none());
    let item = &store.list_outbox_item_details(false).unwrap()[0];
    assert_eq!(item.status, "pending");
    assert_eq!(item.attempt_count, 1);
}

#[tokio::test]
async fn entry_lock_malformed_status_is_retryable() {
    for response in [
        json!({}),
        json!({"lease":{}}),
        json!({"lease":null}),
        json!({"lease":null,"server_time":"bad date"}),
    ] {
        let (_dir, store, profile, api) =
            lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
        let api = api
            .with_json_fixture("GET_JSON /shares/j/entries/e/lock", response)
            .await;
        drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
            .await
            .expect_err("malformed response must not be read as unlocked");
        assert!(api.last_put_entry_content_json().await.is_none());
        assert_eq!(
            store.list_outbox_item_details(false).unwrap()[0].status,
            "pending"
        );
    }
}

#[tokio::test]
async fn entry_lock_gate_skips_personal_strict_and_disabled_journals() {
    for (journal, flags) in [
        (
            json!({"id":"j","is_shared":false,"shared_permissions":"any_participant_full"}),
            json!({"shared-journals-v2":true}),
        ),
        (
            json!({"id":"j","is_shared":0,"shared_permissions":"any_participant_full"}),
            json!({"shared-journals-v2":true}),
        ),
        (
            json!({"id":"j","shared_permissions":"any_participant_full"}),
            json!({"shared-journals-v2":true}),
        ),
        (
            json!({"id":"j","is_shared":true,"shared_permissions":"creator_full+owner_delete"}),
            json!({"shared-journals-v2":true}),
        ),
        (relaxed_lock_journal(), json!({})),
        (relaxed_lock_journal(), json!({"shared-journals-v2":false})),
    ] {
        let (_dir, store, profile, api) = lock_check_fixture(journal, flags).await;
        drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
            .await
            .unwrap();
        assert!(api.last_put_entry_content_json().await.is_some());
    }
}

#[tokio::test]
async fn entry_conflict_failed_payload_survives_entry_and_journal_removal() {
    for remove_journal in [true, false] {
        let (_dir, store, profile, _) =
            lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
        let original = store.list_outbox_item_details(true).unwrap()[0]
            .payload_json
            .clone();
        let api = crate::http::fake::FakeApiClient::new()
            .with_json_fixture(
                "GET_JSON /shares/j/entries/e/lock",
                json!({"lease":null,"server_time":"2026-09-01T00:00:00Z"}),
            )
            .await
            .with_bytes_fixture(
                "PUT_ENTRY_MULTIPART /v3/sync/entries/j/e",
                serde_json::to_vec(&json!({"outcome":"dirty"})).unwrap(),
            )
            .await;
        assert!(
            drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
                .await
                .is_err()
        );
        if remove_journal {
            store.delete_local_journal("j").unwrap();
        } else {
            store.delete_local_entry("j", "e").unwrap();
        }
        let items = store.list_outbox_item_details(true).unwrap();
        assert_eq!(
            items.len(),
            1,
            "deleting a local row must not remove retained failed work"
        );
        assert_eq!(items[0].payload_json, original);
        assert_eq!(items[0].status, "failed");
    }
}

#[tokio::test]
async fn entry_lock_upload_uses_snapshot_timestamp_after_local_row_changes() {
    let (_dir, store, profile, api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    store
        .upsert_entry_json_row_with_edit_date(
            "e",
            "j",
            Some("2026-09-07T00:00:00Z"),
            None,
            None,
            r#"{"id":"e","body":"different newer local row"}"#,
        )
        .unwrap();
    let api = api
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            json!({"lease":null,"server_time":"2026-09-01T00:00:00Z"}),
        )
        .await;
    drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
        .await
        .unwrap();
    assert_eq!(
        api.last_put_entry_envelope().await.unwrap()["editDate"],
        1712757600000_f64
    );
    assert_eq!(
        api.last_put_entry_content_json().await.unwrap()["body"],
        "retained text"
    );
}

#[tokio::test]
async fn entry_snapshot_without_saved_edit_time_does_not_get_a_fresh_timestamp() {
    let (_dir, store, profile, api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    let item = store.list_outbox_item_details(true).unwrap().remove(0);
    let mut payload: Value = serde_json::from_str(item.payload_json.as_deref().unwrap()).unwrap();
    payload
        .as_object_mut()
        .unwrap()
        .remove("queued_at_epoch_ms");
    store
        .enqueue_outbox_item(&item.id, "entry", "update", "j:e", &payload.to_string(), 0)
        .unwrap();
    let api = api
        .with_json_fixture(
            "GET_JSON /shares/j/entries/e/lock",
            json!({"lease":null,"server_time":"2026-09-01T00:00:00Z"}),
        )
        .await;
    drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
        .await
        .expect_err("a retained snapshot must not get the current time on retry");
    assert!(api.last_put_entry_content_json().await.is_none());
    assert_eq!(
        store.list_outbox_item_details(false).unwrap()[0].status,
        "failed"
    );
}

#[tokio::test]
async fn entry_lock_check_does_not_change_entry_deletion() {
    let (_dir, store, profile, api) =
        lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
    let item = store.list_outbox_item_details(true).unwrap().remove(0);
    store
        .enqueue_outbox_item(
            &item.id,
            "entry",
            "delete",
            "j:e",
            item.payload_json.as_deref().unwrap(),
            0,
        )
        .unwrap();
    drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
        .await
        .unwrap();
    assert_eq!(
        api.last_put_entry_envelope().await.unwrap()["type"],
        "delete"
    );
}

#[tokio::test]
async fn entry_lock_ownership_uses_canonical_session_identity() {
    for (user, expected_id) in [
        (json!({"user_id":"owner"}), "owner"),
        (json!({"userId":"owner"}), "owner"),
        (json!({"id":77}), "77"),
        (json!({"id":" owner "}), "owner"),
    ] {
        let (_dir, store, profile, api) =
            lock_check_fixture(relaxed_lock_journal(), json!({"shared-journals-v2":true})).await;
        store
            .connect()
            .unwrap()
            .execute(
                "UPDATE auth_sessions SET user_json = ?1 WHERE profile_id = ?2",
                rusqlite::params![user.to_string(), profile],
            )
            .unwrap();
        let device = crate::http::DeviceInfo::resolve(Some(expected_id));
        let api = api
            .with_json_fixture(
                "GET_JSON /shares/j/entries/e/lock",
                lock_response(expected_id, &device.id),
            )
            .await;
        drain_sync_outbox(&store, &api, 1, profile, &mut vec![], "outbox:post")
            .await
            .unwrap();
        assert!(api.last_put_entry_content_json().await.is_some());
    }
}
