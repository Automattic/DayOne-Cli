use super::super::*;
use super::common::test_store_path;
use crate::entry_embeddings::ENTRY_EMBEDDING_DIMENSION;
use serde_json::Value;

#[test]
fn reset_processing_outbox_items_makes_items_leaseable_again() {
    let path = test_store_path("outbox-reset");
    let store = Store::open_at(&path).expect("store should open");
    store
        .enqueue_outbox_item("context_item:1", "context_item", "update", "1", "{}", 0)
        .expect("enqueue should work");
    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);
    let before_reset = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert!(before_reset.is_empty());
    store
        .reset_processing_outbox_items(i64::MAX)
        .expect("reset should work");
    let after_reset = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(after_reset.len(), 1);
}

#[test]
fn reset_processing_outbox_items_only_resets_stale_rows() {
    let path = test_store_path("outbox-reset-stale-only");
    let store = Store::open_at(&path).expect("store should open");
    store
        .enqueue_outbox_item("entry:stale", "entry", "update", "j1:e1", "{}", 0)
        .expect("enqueue stale should work");
    store
        .enqueue_outbox_item("entry:fresh", "entry", "update", "j1:e2", "{}", 0)
        .expect("enqueue fresh should work");
    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 2);

    let conn = store.connect().expect("connect should work");
    conn.execute(
        "UPDATE sync_outbox SET updated_at = datetime('now', '-2 hours') WHERE id = 'entry:stale'",
        [],
    )
    .expect("backdate stale lease should work");
    conn.execute(
        "UPDATE sync_outbox SET updated_at = CURRENT_TIMESTAMP WHERE id = 'entry:fresh'",
        [],
    )
    .expect("refresh fresh lease should work");

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time should be after unix epoch")
        .as_millis() as i64;
    let stale_cutoff = now_ms - (60 * 60 * 1_000);
    store
        .reset_processing_outbox_items(stale_cutoff)
        .expect("stale reset should work");

    let released = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].id, "entry:stale");
}

#[test]
fn stale_retry_fallback_keeps_existing_backoff_fields() {
    let path = test_store_path("outbox-stale-fallback-preserves-backoff");
    let store = Store::open_at(&path).expect("store should open");
    store
        .enqueue_outbox_item(
            "entry:coalesce",
            "entry",
            "update",
            "j1:e1",
            "{\"v\":1}",
            12345,
        )
        .expect("enqueue should work");
    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].payload_json, "{\"v\":1}");

    // Simulate a newer payload being coalesced while the row is still leased.
    store
        .enqueue_outbox_item(
            "entry:coalesce",
            "entry",
            "update",
            "j1:e1",
            "{\"v\":2}",
            67890,
        )
        .expect("coalesced enqueue should work");

    let conn = store.connect().expect("connect should work");
    conn.execute(
        "UPDATE sync_outbox SET attempt_count = 4, next_attempt_epoch_ms = 67890 WHERE id = 'entry:coalesce'",
        [],
    )
    .expect("seed backoff should work");

    // Old lease payload no longer matches; fallback should only release processing->pending.
    store
        .mark_outbox_item_retry("entry:coalesce", "{\"v\":1}", i64::MAX, Some("stale lease"))
        .expect("retry fallback should work");

    let (attempt_count, next_attempt): (i64, i64) = conn
        .query_row(
            "SELECT attempt_count, next_attempt_epoch_ms FROM sync_outbox WHERE id = 'entry:coalesce'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query should work");
    assert_eq!(attempt_count, 4);
    assert_eq!(next_attempt, 67890);
}

#[test]
fn defer_does_not_increment_attempt_count() {
    let path = test_store_path("outbox-defer-preserves-attempt");
    let store = Store::open_at(&path).expect("store should open");
    store
        .enqueue_outbox_item(
            "daily_chat:defer",
            "daily_chat_feed",
            "update",
            "2026-03-17",
            "{}",
            100,
        )
        .expect("enqueue should work");
    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);

    let conn = store.connect().expect("connect should work");
    conn.execute(
        "UPDATE sync_outbox SET attempt_count = 3 WHERE id = 'daily_chat:defer'",
        [],
    )
    .expect("seed attempt count should work");

    store
        .mark_outbox_item_deferred(
            "daily_chat:defer",
            "{}",
            123_456,
            Some("waiting for daily chat keys"),
        )
        .expect("defer should work");

    let (attempt_count, next_attempt, last_error): (i64, i64, Option<String>) = conn
        .query_row(
            "SELECT attempt_count, next_attempt_epoch_ms, last_error FROM sync_outbox WHERE id = 'daily_chat:defer'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query should work");
    assert_eq!(attempt_count, 3);
    assert_eq!(next_attempt, 123_456);
    assert_eq!(last_error.as_deref(), Some("waiting for daily chat keys"));
}

#[test]
fn stale_defer_fallback_sets_backoff_and_error_fields() {
    let path = test_store_path("outbox-defer-fallback-updates-fields");
    let store = Store::open_at(&path).expect("store should open");
    store
        .enqueue_outbox_item(
            "daily_chat:coalesce",
            "daily_chat_feed",
            "update",
            "2026-03-17",
            "{\"v\":1}",
            0,
        )
        .expect("enqueue should work");
    let leased = store
        .lease_outbox_items(i64::MAX, 10)
        .expect("lease should work");
    assert_eq!(leased.len(), 1);

    // Simulate a newer payload being coalesced while row is leased.
    store
        .enqueue_outbox_item(
            "daily_chat:coalesce",
            "daily_chat_feed",
            "update",
            "2026-03-17",
            "{\"v\":2}",
            10,
        )
        .expect("coalesced enqueue should work");

    store
        .mark_outbox_item_deferred(
            "daily_chat:coalesce",
            "{\"v\":1}",
            123_000,
            Some("waiting for keys"),
        )
        .expect("defer fallback should work");

    let conn = store.connect().expect("connect should work");
    let (status, next_attempt, last_error): (String, i64, Option<String>) = conn
        .query_row(
            "SELECT status, next_attempt_epoch_ms, last_error FROM sync_outbox WHERE id = 'daily_chat:coalesce'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query should work");
    assert_eq!(status, "pending");
    assert_eq!(next_attempt, 123_000);
    assert_eq!(last_error.as_deref(), Some("waiting for keys"));
}

#[test]
fn entry_embedding_roundtrip_and_join_lookup() {
    let path = test_store_path("entry-embeddings");
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_entry_json_row(
            "entry-embed-1",
            "journal-embed",
            Some("2026-03-18T00:00:00.000Z"),
            None,
            r#"{"id":"entry-embed-1","journal_id":"journal-embed","body":"hello"}"#,
        )
        .expect("entry should save");
    store
        .upsert_entry_embedding("entry-embed-1", "journal-embed", "hash-1", &[0.1, 0.2, 0.3])
        .expect("embedding should save");

    let loaded_hash = store
        .get_entry_embedding_source_hash("entry-embed-1")
        .expect("hash query should work");
    assert_eq!(loaded_hash.as_deref(), Some("hash-1"));

    let rows = store
        .list_entries_with_embeddings(Some("journal-embed"), None)
        .expect("join lookup should work");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].1.is_some());
    assert_eq!(rows[0].1.as_ref().expect("embedding should exist").len(), 3);
}

#[test]
fn semantic_search_converts_l2_distance_to_cosine_similarity() {
    let path = test_store_path("entry-embeddings-l2-cosine-score");
    let store = Store::open_at(&path).expect("store should open");
    for entry_id in ["entry-x", "entry-y"] {
        let row = format!(r#"{{"id":"{entry_id}","journal_id":"journal-embed","body":"hello"}}"#);
        store
            .upsert_entry_json_row(
                entry_id,
                "journal-embed",
                Some("2026-03-18T00:00:00.000Z"),
                None,
                &row,
            )
            .expect("entry should save");
    }

    let mut x_axis = vec![0.0; ENTRY_EMBEDDING_DIMENSION];
    x_axis[0] = 1.0;
    let mut y_axis = vec![0.0; ENTRY_EMBEDDING_DIMENSION];
    y_axis[1] = 1.0;
    store
        .upsert_entry_embedding("entry-x", "journal-embed", "hash-x", &x_axis)
        .expect("x embedding should save");
    store
        .upsert_entry_embedding("entry-y", "journal-embed", "hash-y", &y_axis)
        .expect("y embedding should save");

    let mut query = vec![0.0; ENTRY_EMBEDDING_DIMENSION];
    let half_sqrt_two = 1.0_f64 / 2.0_f64.sqrt();
    query[0] = half_sqrt_two;
    query[1] = half_sqrt_two;

    let rows = store
        .search_entry_semantic_scores(&query, 0.6, Some("journal-embed"))
        .expect("semantic search should work");
    let ids = rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, vec!["entry-x", "entry-y"]);
    for (_, similarity) in rows {
        assert!(
            (similarity - half_sqrt_two).abs() < 0.0001,
            "expected cosine-ish similarity near {half_sqrt_two}, got {similarity}"
        );
    }
}

#[test]
fn list_entries_with_embeddings_excludes_deleted_entries() {
    let path = test_store_path("entry-embeddings-exclude-deleted");
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_entry_json_row(
            "entry-active",
            "journal-embed",
            Some("2026-03-18T00:00:00.000Z"),
            None,
            r#"{"id":"entry-active","journal_id":"journal-embed","body":"hello"}"#,
        )
        .expect("active entry should save");
    store
        .upsert_entry_embedding("entry-active", "journal-embed", "hash-active", &[0.1, 0.2])
        .expect("active embedding should save");
    store
        .upsert_entry_json_row(
            "entry-deleted",
            "journal-embed",
            Some("2026-03-18T00:01:00.000Z"),
            Some("2026-03-18T01:00:00.000Z"),
            r#"{"id":"entry-deleted","journal_id":"journal-embed","deleted_at":"2026-03-18T01:00:00.000Z","body":"bye"}"#,
        )
        .expect("deleted entry should save");
    store
        .upsert_entry_embedding(
            "entry-deleted",
            "journal-embed",
            "hash-deleted",
            &[0.3, 0.4],
        )
        .expect("deleted embedding should save");

    let rows = store
        .list_entries_with_embeddings(Some("journal-embed"), None)
        .expect("join lookup should work");
    assert_eq!(rows.len(), 1);
    let active_row: Value =
        serde_json::from_str(&rows[0].0).expect("stored row should decode as JSON");
    assert_eq!(
        active_row.get("id").and_then(Value::as_str),
        Some("entry-active")
    );
}

#[test]
fn journal_update_is_gated_behind_pending_journal_create() {
    // A journal:update for a not-yet-synced journal must not lease (and PUT to a
    // `pending-…` id) while its create is still queued. It becomes leasable only
    // after the create completes (by which point the id has been remapped).
    let path = test_store_path("outbox-journal-update-gated");
    let store = Store::open_at(&path).expect("store should open");
    store
        .enqueue_outbox_item(
            "journal:pending-x",
            "journal",
            "create",
            "pending-x",
            "{}",
            0,
        )
        .expect("enqueue create");
    store
        .enqueue_outbox_item(
            "journal:update:pending-x",
            "journal",
            "update",
            "pending-x",
            "{}",
            0,
        )
        .expect("enqueue update");

    // Only the create leases; the update is gated behind it.
    let leased = store.lease_outbox_items(i64::MAX, 10).expect("lease");
    let ids: Vec<&str> = leased.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec!["journal:pending-x"]);

    // Complete the create (leased payload is the enqueued "{}").
    store
        .mark_outbox_item_succeeded("journal:pending-x", "{}")
        .expect("mark create succeeded");

    // With the create gone, the update is now leasable.
    let leased = store.lease_outbox_items(i64::MAX, 10).expect("lease");
    let ids: Vec<&str> = leased.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec!["journal:update:pending-x"]);
}
