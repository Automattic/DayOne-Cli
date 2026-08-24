use super::super::*;
use super::common::test_store_path;
use crate::entry_embeddings::ENTRY_EMBEDDING_DIMENSION;
use rusqlite::Connection;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde_json::{Value, json};

#[cfg(unix)]
#[test]
fn store_secures_database_file_and_directory() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("temp dir should create");
    let parent = root.path().join("profiles").join("production");
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = parent.join("dayone.db");

    let _store = Store::open_for_profile(root.path(), "production", PRODUCTION_URL)
        .expect("store should open");

    assert_eq!(
        std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn schema_migrations_table_tracks_all_versions() {
    let path = test_store_path("migrations-versioned");
    let _store = Store::open_at(&path).expect("store should open");
    let conn = Connection::open(&path).expect("raw connection should open");

    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .expect("prepare should succeed")
        .query_map([], |row| row.get(0))
        .expect("query should succeed")
        .collect::<Result<_, _>>()
        .expect("rows should be valid");

    let expected: Vec<i64> = MIGRATIONS.iter().map(|(v, _)| *v).collect();
    assert_eq!(versions, expected);
}

#[test]
fn migration_0017_preserves_profile_data_and_requests_inspection() {
    let path = test_store_path("migration-0017-journal-metadata");
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_json_row(
            "journals",
            "journal-existing",
            None,
            None,
            r#"{"id":"journal-existing","name":"Existing"}"#,
        )
        .expect("existing journal should save");
    store
        .set_sync_cursor("journals", Some("existing-cursor"))
        .expect("existing cursor should save");
    store
        .enqueue_outbox_item(
            "journal:update:journal-existing",
            "journal",
            "update",
            "journal-existing",
            "{}",
            0,
        )
        .expect("existing outbox item should save");
    store
        .connect()
        .expect("connection should open")
        .execute_batch(
            "DROP TABLE journal_metadata_diagnostics;
             DROP TABLE journal_metadata_inspection;
             DELETE FROM schema_migrations WHERE version = 17;",
        )
        .expect("version 16 fixture should save");
    drop(store);

    let store = Store::open_at(&path).expect("migration 17 should run");
    assert!(
        store
            .get_json_row_by_id("journals", "journal-existing")
            .expect("journal should load")
            .is_some()
    );
    assert_eq!(
        store
            .get_sync_cursor("journals")
            .expect("cursor should load")
            .as_deref(),
        Some("existing-cursor")
    );
    assert_eq!(
        store
            .connect()
            .expect("connection should open")
            .query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row
                .get::<_, i64>(0))
            .expect("outbox count should load"),
        1
    );
    assert!(
        store
            .start_journal_metadata_inspection()
            .expect("inspection request should start")
    );
    assert_eq!(
        store
            .get_sync_cursor("journals")
            .expect("journal cursor should load"),
        None
    );
}

#[test]
fn journal_metadata_diagnostic_schema_rejects_private_values_and_unknown_states() {
    let path = test_store_path("journal-metadata-diagnostics");
    let store = Store::open_at(&path).expect("store should open");
    let connection = store.connect().expect("connection should open");

    store
        .replace_journal_metadata_diagnostics(
            "journal-1",
            &[("name", "plaintext"), ("description_v2", "unverified_d1")],
        )
        .expect("diagnostics should save");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM journal_metadata_diagnostics WHERE journal_id = 'journal-1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count should load"),
        2
    );
    assert!(
        connection
            .execute(
                "INSERT INTO journal_metadata_diagnostics (journal_id, field, status) VALUES ('journal-2', 'name', 'Private Journal')",
                [],
            )
            .is_err(),
        "the status constraint must reject private metadata values"
    );
    let snapshot = store
        .journal_metadata_diagnostics("journal-1")
        .expect("diagnostics should load");
    assert!(
        store
            .replace_journal_metadata_diagnostics("journal-1", &[("title", "plaintext")])
            .is_err(),
        "unsupported fields must be rejected"
    );
    assert!(
        store
            .replace_journal_metadata_diagnostics("journal-1", &[("name", "Private Journal")])
            .is_err(),
        "private metadata must not be accepted as a status"
    );
    assert_eq!(
        store
            .journal_metadata_diagnostics("journal-1")
            .expect("diagnostics should load"),
        snapshot,
        "rejected diagnostics must not change existing rows"
    );

    store
        .replace_journal_metadata_diagnostics("journal-1", &[])
        .expect("diagnostics should clear");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM journal_metadata_diagnostics",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count should load"),
        0
    );
}

#[test]
fn pulled_journal_row_and_diagnostics_commit_atomically() {
    let path = test_store_path("pulled-journal-atomic");
    let store = Store::open_at(&path).expect("store should open");
    store
        .connect()
        .expect("connection should open")
        .execute_batch(
            "CREATE TRIGGER fail_journal_sync_info
             BEFORE INSERT ON journal_sync_info
             WHEN NEW.journal_id = 'journal-atomic'
             BEGIN
               SELECT RAISE(ABORT, 'forced test failure');
             END;",
        )
        .expect("failure trigger should save");

    assert!(
        store
            .apply_pulled_journal(
                "journal-atomic",
                None,
                None,
                r#"{"id":"journal-atomic","name":"Server value"}"#,
                &["name"],
                &[("name", "plaintext")],
            )
            .is_err()
    );
    assert!(
        store
            .get_json_row_by_id("journals", "journal-atomic")
            .expect("journal should load")
            .is_none(),
        "a later transaction failure must roll back the journal row"
    );
    assert!(
        store
            .journal_metadata_diagnostics("journal-atomic")
            .expect("diagnostics should load")
            .is_empty(),
        "a later transaction failure must roll back diagnostics"
    );
}

#[test]
fn local_journal_update_and_outbox_enqueue_commit_atomically() {
    let path = test_store_path("local-journal-update-atomic");
    let store = Store::open_at(&path).expect("store should open");
    let original = r#"{"id":"journal-atomic","name":"Original"}"#;
    store
        .upsert_json_row("journals", "journal-atomic", None, None, original)
        .expect("original journal should save");
    store
        .connect()
        .expect("connection should open")
        .execute_batch(
            "CREATE TRIGGER fail_journal_outbox
             BEFORE INSERT ON sync_outbox
             WHEN NEW.object_id = 'journal-atomic'
             BEGIN
               SELECT RAISE(ABORT, 'forced test failure');
             END;",
        )
        .expect("failure trigger should save");

    assert!(
        store
            .upsert_journal_and_enqueue_update(
                "journal-atomic",
                None,
                None,
                r#"{"id":"journal-atomic","name":"Local edit"}"#,
            )
            .is_err()
    );
    assert_eq!(
        store
            .get_json_row_by_id("journals", "journal-atomic")
            .expect("journal should load")
            .as_deref(),
        Some(original),
        "an outbox failure must roll back the local journal edit"
    );
}

#[test]
fn current_journal_update_response_replaces_the_local_row() {
    let path = test_store_path("current-journal-update-response");
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_journal_and_enqueue_update(
            "journal-current",
            None,
            None,
            r#"{"id":"journal-current","name":"Local edit"}"#,
        )
        .expect("local edit should queue");
    let leased = store
        .lease_outbox_items(0, 10)
        .expect("journal update should lease")
        .pop()
        .expect("leased journal update should exist");

    assert!(
        store
            .apply_journal_update_response_if_current(
                &leased.id,
                &leased.payload_json,
                "journal-current",
                None,
                None,
                r#"{"id":"journal-current","name":"Server response"}"#,
            )
            .expect("current response should apply")
    );
    assert_eq!(
        store
            .get_json_row_by_id("journals", "journal-current")
            .expect("journal should load")
            .as_deref(),
        Some(r#"{"id":"journal-current","name":"Server response"}"#)
    );
}

#[test]
fn journal_edit_during_push_requeues_the_latest_row() {
    let path = test_store_path("journal-update-during-push");
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_journal_and_enqueue_update(
            "journal-race",
            None,
            None,
            r#"{"id":"journal-race","name":"First edit"}"#,
        )
        .expect("first edit should queue");
    let leased = store
        .lease_outbox_items(0, 10)
        .expect("journal update should lease")
        .pop()
        .expect("leased journal update should exist");

    store
        .upsert_journal_and_enqueue_update(
            "journal-race",
            None,
            None,
            r#"{"id":"journal-race","name":"Second edit"}"#,
        )
        .expect("second edit should save while first push is processing");
    assert!(
        !store
            .apply_journal_update_response_if_current(
                &leased.id,
                &leased.payload_json,
                "journal-race",
                None,
                None,
                r#"{"id":"journal-race","name":"First server response"}"#,
            )
            .expect("stale server response should be checked"),
        "a stale push response must not replace the newer local row"
    );
    store
        .mark_outbox_item_succeeded(&leased.id, &leased.payload_json)
        .expect("first push should settle");

    let (status, payload): (String, String) = store
        .connect()
        .expect("connection should open")
        .query_row(
            "SELECT status, payload_json FROM sync_outbox WHERE id = ?1",
            [&leased.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("latest marker should remain queued");
    assert_eq!(status, "pending");
    assert_ne!(payload, leased.payload_json);
    assert_eq!(
        store
            .get_json_row_by_id("journals", "journal-race")
            .expect("journal should load")
            .as_deref(),
        Some(r#"{"id":"journal-race","name":"Second edit"}"#)
    );
}

#[test]
fn user_identity_key_prefers_authoritative_endpoint_payload() {
    let path = test_store_path("user-identity-key-source");
    let store = Store::open_at(&path).expect("store should open");

    store
        .upsert_singleton_json_row("user_keys", 1, r#"{"source":"legacy-bundle"}"#, None)
        .expect("legacy key bundle should save");
    assert_eq!(
        store
            .get_user_identity_key_json()
            .expect("legacy fallback should load")
            .as_deref(),
        Some(r#"{"source":"legacy-bundle"}"#)
    );

    store
        .upsert_singleton_json_row(
            "user_identity_key",
            1,
            r#"{"source":"users-key-endpoint"}"#,
            None,
        )
        .expect("authoritative user key should save");
    assert_eq!(
        store
            .get_user_identity_key_json()
            .expect("authoritative key should load")
            .as_deref(),
        Some(r#"{"source":"users-key-endpoint"}"#)
    );
}

#[test]
fn unread_entries_table_includes_deleted_columns_after_migration() {
    let path = test_store_path("unread-entries-columns");
    let _store = Store::open_at(&path).expect("store should open");
    let conn = Connection::open(&path).expect("raw connection should open");

    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(unread_entries)")
        .expect("pragma prepare should succeed")
        .query_map([], |row| row.get(1))
        .expect("pragma query should succeed")
        .collect::<Result<_, _>>()
        .expect("column rows should parse");

    assert!(
        columns.iter().any(|name| name == "deleted_at"),
        "unread_entries should include deleted_at"
    );
    assert!(
        columns.iter().any(|name| name == "is_deleted"),
        "unread_entries should include is_deleted"
    );
}

#[test]
fn migration_0012_scrubs_legacy_decrypted_text_from_existing_entries() {
    let path = test_store_path("migration-0012-decrypted-text");
    let store = Store::open_at(&path).expect("store should open");
    drop(store);

    let conn = Connection::open(&path).expect("raw connection should open");
    conn.execute(
        r#"
        INSERT INTO entries (
          id, journal_id, is_deleted, body, decrypted_text, extra_fields_json, data_json
        ) VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6)
        "#,
        params![
            "entry-legacy-decrypted-text",
            "journal-1",
            "real body",
            "legacy column fallback",
            r#"{"decrypted_text":"legacy extra","weather":{"conditionCode":"clear"}}"#,
            r#"{"id":"entry-legacy-decrypted-text","body":"real body","decrypted_text":"legacy root fallback","payload":{"body":"payload body","decrypted_text":"legacy payload fallback"}}"#,
        ],
    )
    .expect("legacy entry should insert");
    conn.execute("DELETE FROM schema_migrations WHERE version = 12", [])
        .expect("migration marker should delete");
    drop(conn);

    let _store = Store::open_at(&path).expect("store reopen should run migration 0012");
    let conn = Connection::open(&path).expect("raw connection should reopen");
    let (decrypted_text, extra_fields_json, data_json): (Option<String>, Option<String>, String) =
        conn.query_row(
            "SELECT decrypted_text, extra_fields_json, data_json FROM entries WHERE id = ?1",
            params!["entry-legacy-decrypted-text"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("entry should query");
    let stored: Value = serde_json::from_str(&data_json).expect("data_json should parse");
    let extra: Value = serde_json::from_str(
        extra_fields_json
            .as_deref()
            .expect("non-dropped extra field should remain"),
    )
    .expect("extra_fields_json should parse");

    assert_eq!(decrypted_text, None);
    assert_eq!(stored.get("decrypted_text"), None);
    assert_eq!(stored.pointer("/payload/decrypted_text"), None);
    assert_eq!(
        stored.pointer("/payload/body"),
        Some(&json!("payload body"))
    );
    assert_eq!(extra.get("decrypted_text"), None);
    assert_eq!(
        extra.pointer("/weather/conditionCode"),
        Some(&json!("clear"))
    );
}

#[test]
fn migration_0014_rescrubs_legacy_decrypted_text_after_revert_window() {
    let path = test_store_path("migration-0014-decrypted-text-rescrub");
    let store = Store::open_at(&path).expect("store should open");
    drop(store);

    let conn = Connection::open(&path).expect("raw connection should open");
    conn.execute(
        r#"
        INSERT INTO entries (
          id, journal_id, is_deleted, body, decrypted_text, extra_fields_json, data_json
        ) VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6)
        "#,
        params![
            "entry-revert-window-decrypted-text",
            "journal-1",
            "real body",
            "reintroduced column fallback",
            r#"{"decrypted_text":"reintroduced extra","weather":{"conditionCode":"clear"}}"#,
            r#"{"id":"entry-revert-window-decrypted-text","body":"real body","decrypted_text":"reintroduced root fallback","payload":{"body":"payload body","decrypted_text":"reintroduced payload fallback"}}"#,
        ],
    )
    .expect("reintroduced legacy entry should insert");
    conn.execute("DELETE FROM schema_migrations WHERE version = 14", [])
        .expect("migration marker should delete");
    drop(conn);

    let _store = Store::open_at(&path).expect("store reopen should run migration 0014");
    let conn = Connection::open(&path).expect("raw connection should reopen");
    let (decrypted_text, extra_fields_json, data_json): (Option<String>, Option<String>, String) =
        conn.query_row(
            "SELECT decrypted_text, extra_fields_json, data_json FROM entries WHERE id = ?1",
            params!["entry-revert-window-decrypted-text"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("entry should query");
    let stored: Value = serde_json::from_str(&data_json).expect("data_json should parse");
    let extra: Value = serde_json::from_str(
        extra_fields_json
            .as_deref()
            .expect("non-dropped extra field should remain"),
    )
    .expect("extra_fields_json should parse");

    assert_eq!(decrypted_text, None);
    assert_eq!(stored.get("decrypted_text"), None);
    assert_eq!(stored.pointer("/payload/decrypted_text"), None);
    assert_eq!(extra.get("decrypted_text"), None);
    assert_eq!(
        extra.pointer("/weather/conditionCode"),
        Some(&json!("clear"))
    );
}

#[test]
fn reopening_store_is_idempotent() {
    let path = test_store_path("migrations-reopen");
    let store = Store::open_at(&path).expect("first open should succeed");
    store
        .upsert_json_row("journals", "j-1", None, None, r#"{"id":"j-1"}"#)
        .expect("insert should succeed");
    drop(store);

    // Re-open — migrations should be skipped, existing data preserved.
    let store2 = Store::open_at(&path).expect("second open should succeed");
    let rows = store2
        .list_json_rows("journals")
        .expect("list should succeed");
    assert_eq!(rows.len(), 1);

    let conn = Connection::open(&path).expect("raw connection should open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count should succeed");
    assert_eq!(
        count,
        MIGRATIONS.len() as i64,
        "migration count should not change on re-open"
    );
}

#[test]
fn journal_mode_is_wal() {
    let path = test_store_path("wal-mode");
    let _store = Store::open_at(&path).expect("store should open");
    let conn = Connection::open(&path).expect("raw connection should open");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal_mode pragma should succeed");
    assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn store_open_succeeds_when_wal_set_is_temporarily_busy() {
    let path = test_store_path("wal-mode-busy-open");
    let store = Store::open_at(&path).expect("initial store open should succeed");
    drop(store);

    let mut lock_conn = Connection::open(&path).expect("raw lock connection should open");
    let tx = lock_conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("immediate transaction should acquire lock");

    let conn = Connection::open(&path).expect("second raw connection should open");
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
        .expect("busy timeout should configure");
    let configure = configure_sqlite_connection(&conn);
    assert!(
        configure.is_ok(),
        "WAL configuration should handle transient busy"
    );

    tx.rollback().expect("rollback should release lock");
}

#[test]
fn defaults_to_staging_profile() {
    let path = test_store_path("defaults");
    let store = Store::open_at(&path).expect("store should open");
    let active = store
        .get_active_profile()
        .expect("active profile should load")
        .expect("active profile should exist");
    assert_eq!(active.name, DEFAULT_PROFILE_NAME);
}

#[test]
fn upsert_and_load_session_for_base_url() {
    let path = test_store_path("session");
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url(STAGING_URL)
        .expect("profile should resolve");
    store
        .save_auth_session(profile.id, "token-1", "2026-03-06T00:00:00Z", "{\"id\":1}")
        .expect("session should save");

    let session = store
        .get_auth_session_for_base_url(STAGING_URL)
        .expect("session query should succeed")
        .expect("session should exist");
    assert_eq!(session.token, "token-1");
    assert_eq!(session.profile_id, profile.id);
}

#[test]
fn save_auth_session_requires_a_user_id() {
    let path = test_store_path("session-missing-user-id");
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url(STAGING_URL)
        .expect("profile should resolve");

    let err = store
        .save_auth_session(profile.id, "token", "2026-03-06T00:00:00Z", "{}")
        .expect_err("session without a user id should fail");
    assert!(matches!(
        err,
        StoreError::InvalidAuthSessionUserId { profile_id } if profile_id == profile.id
    ));
    assert!(
        store
            .get_auth_session_for_base_url(STAGING_URL)
            .expect("session query should succeed")
            .is_none()
    );
}

#[test]
fn save_auth_session_refreshes_the_same_account_but_rejects_replacement() {
    let path = test_store_path("session-account-switch");
    let store = Store::open_at(&path).expect("store should open");
    let profile = store
        .get_or_create_profile_for_base_url(STAGING_URL)
        .expect("profile should resolve");
    store
        .save_auth_session(
            profile.id,
            "token-1",
            "2026-03-06T00:00:00Z",
            r#"{"id":"user-1"}"#,
        )
        .expect("first session should save");
    store
        .save_auth_session(
            profile.id,
            "token-refreshed",
            "2026-03-07T00:00:00Z",
            r#"{"user_id":"user-1"}"#,
        )
        .expect("same-account session should refresh");

    let err = store
        .save_auth_session(
            profile.id,
            "token-2",
            "2026-03-08T00:00:00Z",
            r#"{"id":"user-2"}"#,
        )
        .expect_err("account replacement should require logout");
    assert!(matches!(
        err,
        StoreError::AuthSessionAccountMismatch { profile_id } if profile_id == profile.id
    ));
    let session = store
        .get_auth_session_for_base_url(STAGING_URL)
        .expect("session query should succeed")
        .expect("same-account session should remain");
    assert_eq!(session.token, "token-refreshed");
    assert_eq!(session.token_created_at, "2026-03-07T00:00:00Z");
    assert_eq!(session.user_json, r#"{"user_id":"user-1"}"#);
}

#[test]
fn sync_cursor_persists_across_calls() {
    let path = test_store_path("cursor");
    let store = Store::open_at(&path).expect("store should open");
    assert!(
        store
            .get_sync_cursor("entries:test")
            .expect("cursor should query")
            .is_none()
    );
    store
        .set_sync_cursor("entries:test", Some("cursor-1"))
        .expect("cursor should save");
    let loaded = store
        .get_sync_cursor("entries:test")
        .expect("cursor should query");
    assert_eq!(loaded.as_deref(), Some("cursor-1"));
}

#[test]
fn clear_sync_cursors_removes_all_rows() {
    let path = test_store_path("clear-cursors");
    let store = Store::open_at(&path).expect("store should open");
    store
        .set_sync_cursor("entries:1", Some("cursor-1"))
        .expect("first cursor should save");
    store
        .set_sync_cursor("entries:2", Some("cursor-2"))
        .expect("second cursor should save");

    store
        .clear_sync_cursors()
        .expect("all cursors should clear");

    assert!(
        store
            .get_sync_cursor("entries:1")
            .expect("first cursor should query")
            .is_none()
    );
    assert!(
        store
            .get_sync_cursor("entries:2")
            .expect("second cursor should query")
            .is_none()
    );
}

#[test]
fn delete_local_journal_removes_journal_scoped_rows_only() {
    let path = test_store_path("delete-local-journal");
    let store = Store::open_at(&path).expect("store should open");
    seed_journal_scoped_rows(&store, "j1", "e1", "c1", "m1");
    seed_journal_scoped_rows(&store, "j10", "e10", "c10", "m10");
    store
        .set_sync_cursor("entries:j1", Some("cursor-target"))
        .expect("target entry cursor should save");
    store
        .set_sync_cursor("comments:j1:e1", Some("cursor-comment"))
        .expect("target comments cursor should save");
    store
        .set_sync_cursor("entries:j10", Some("cursor-other"))
        .expect("other entry cursor should save");
    store
        .enqueue_outbox_item(
            "journal:update:j1",
            "journal",
            "update",
            "j1",
            r#"{"id":"j1","name":"pending update"}"#,
            0,
        )
        .expect("target journal outbox should save");
    store
        .enqueue_outbox_item(
            "entry:j1:e1",
            "entry",
            "update",
            "j1:e1",
            r#"{"journal_id":"j1","entry_id":"e1"}"#,
            0,
        )
        .expect("target entry outbox should save");
    store
        .enqueue_outbox_item(
            "media:j1:e1:m1",
            "original_media",
            "create",
            "j1:e1:m1",
            r#"{"journal_id":"j1","entry_id":"e1","moment_id":"m1"}"#,
            0,
        )
        .expect("target media outbox should save");
    store
        .enqueue_outbox_item(
            "comment:j1:e1:c1",
            "comment",
            "update",
            "j1:e1:c1",
            r#"{"journal_id":"j1","entry_id":"e1","id":"c1"}"#,
            0,
        )
        .expect("target comment outbox should save");
    store
        .enqueue_outbox_item(
            "entry:j10:e10",
            "entry",
            "update",
            "j10:e10",
            r#"{"journal_id":"j10","entry_id":"e10"}"#,
            0,
        )
        .expect("other outbox should save");

    store
        .delete_local_journal("j1")
        .expect("target journal should delete");

    let conn = store.connect().expect("connection should open");
    for (table, column) in JOURNAL_SCOPED_TABLES {
        let count = count_rows_by_column(&conn, table, column, "j1");
        assert_eq!(count, 0, "{table} rows for deleted journal should be gone");
    }
    assert!(
        store
            .get_sync_cursor("entries:j1")
            .expect("target cursor should query")
            .is_none()
    );
    assert!(
        store
            .get_sync_cursor("comments:j1:e1")
            .expect("target comments cursor should query")
            .is_none()
    );
    let target_outbox_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_outbox WHERE object_id = 'j1' OR object_id LIKE 'j1:%'",
            [],
            |row| row.get(0),
        )
        .expect("target outbox count should work");
    assert_eq!(target_outbox_count, 0);

    assert!(
        store
            .get_json_row_by_id("journals", "j10")
            .expect("other journal should query")
            .is_some(),
        "unrelated journal should remain"
    );
    assert!(
        store
            .get_entry_json_row_with_extra_fields("e10")
            .expect("other entry should query")
            .is_some(),
        "unrelated entry should remain"
    );
    assert_eq!(
        store
            .get_sync_cursor("entries:j10")
            .expect("other cursor should query")
            .as_deref(),
        Some("cursor-other")
    );
    for (table, column) in JOURNAL_SCOPED_TABLES {
        let count = count_rows_by_column(&conn, table, column, "j10");
        assert_eq!(count, 1, "{table} rows for unrelated journal should remain");
    }
    let other_outbox_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_outbox WHERE object_id = 'j10:e10'",
            [],
            |row| row.get(0),
        )
        .expect("other outbox count should work");
    assert_eq!(other_outbox_count, 1);
}

#[test]
fn delete_local_entry_removes_entry_scoped_rows_only() {
    let path = test_store_path("delete-local-entry");
    let store = Store::open_at(&path).expect("store should open");
    seed_journal_scoped_rows(&store, "j1", "e1", "c1", "m1");
    seed_journal_scoped_rows(&store, "j1", "e10", "c10", "m10");
    store
        .set_sync_cursor("entries:j1", Some("entries-cursor"))
        .expect("entries cursor should save");
    store
        .set_sync_cursor("comments:j1:e1", Some("target-comment-cursor"))
        .expect("target comments cursor should save");
    store
        .set_sync_cursor("comments:j1:e10", Some("other-comment-cursor"))
        .expect("other comments cursor should save");
    store
        .enqueue_outbox_item(
            "entry:j1:e1",
            "entry",
            "update",
            "j1:e1",
            r#"{"journal_id":"j1","entry_id":"e1"}"#,
            0,
        )
        .expect("target entry outbox should save");
    store
        .enqueue_outbox_item(
            "media:j1:e1:m1",
            "original_media",
            "create",
            "j1:e1:m1",
            r#"{"journal_id":"j1","entry_id":"e1","moment_id":"m1"}"#,
            0,
        )
        .expect("target media outbox should save");
    store
        .enqueue_outbox_item(
            "comment:j1:e1:c1",
            "comment",
            "update",
            "j1:e1:c1",
            r#"{"journal_id":"j1","entry_id":"e1","id":"c1"}"#,
            0,
        )
        .expect("target comment outbox should save");
    store
        .enqueue_outbox_item(
            "entry:j1:e10",
            "entry",
            "update",
            "j1:e10",
            r#"{"journal_id":"j1","entry_id":"e10"}"#,
            0,
        )
        .expect("other entry outbox should save");

    store
        .delete_local_entry("j1", "e1")
        .expect("target entry should delete");

    let conn = store.connect().expect("connection should open");
    for (table, column) in ENTRY_SCOPED_TABLES {
        let count = count_rows_by_column(&conn, table, column, "e1");
        assert_eq!(count, 0, "{table} rows for deleted entry should be gone");
    }
    assert!(
        store
            .get_sync_cursor("comments:j1:e1")
            .expect("target comments cursor should query")
            .is_none()
    );
    let target_outbox_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_outbox WHERE object_id = 'j1:e1' OR object_id LIKE 'j1:e1:%'",
            [],
            |row| row.get(0),
        )
        .expect("target outbox count should work");
    assert_eq!(target_outbox_count, 0);

    assert!(
        store
            .get_json_row_by_id("journals", "j1")
            .expect("journal should query")
            .is_some(),
        "entry deletion should not delete its journal"
    );
    assert_eq!(
        store
            .get_sync_cursor("entries:j1")
            .expect("entries cursor should query")
            .as_deref(),
        Some("entries-cursor")
    );
    assert_eq!(
        store
            .get_sync_cursor("comments:j1:e10")
            .expect("other comments cursor should query")
            .as_deref(),
        Some("other-comment-cursor")
    );
    for (table, column) in ENTRY_SCOPED_TABLES {
        let count = count_rows_by_column(&conn, table, column, "e10");
        assert_eq!(count, 1, "{table} rows for unrelated entry should remain");
    }
    let other_outbox_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_outbox WHERE object_id = 'j1:e10'",
            [],
            |row| row.get(0),
        )
        .expect("other outbox count should work");
    assert_eq!(other_outbox_count, 1);
}

const ENTRY_SCOPED_TABLES: &[(&str, &str)] = &[
    ("entries", "id"),
    ("comments", "entry_id"),
    ("comment_reactions", "entry_id"),
    ("entry_attachments", "entry_id"),
    ("entry_embeddings", "entry_id"),
    ("entry_embeddings_vec", "entry_id"),
    ("unread_entries", "entry_id"),
];

const JOURNAL_SCOPED_TABLES: &[(&str, &str)] = &[
    ("journals", "id"),
    ("entries", "journal_id"),
    ("comments", "journal_id"),
    ("comment_reactions", "journal_id"),
    ("entry_attachments", "journal_id"),
    ("entry_embeddings", "journal_id"),
    ("entry_embeddings_vec", "journal_id"),
    ("unread_entries", "journal_id"),
    ("journal_metadata_diagnostics", "journal_id"),
    ("journal_sync_info", "journal_id"),
];

fn count_rows_by_column(conn: &Connection, table: &str, column: &str, value: &str) -> i64 {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
        params![value],
        |row| row.get(0),
    )
    .expect("count query should work")
}

fn seed_journal_scoped_rows(
    store: &Store,
    journal_id: &str,
    entry_id: &str,
    comment_id: &str,
    moment_id: &str,
) {
    store
        .upsert_json_row(
            "journals",
            journal_id,
            None,
            None,
            &format!(r#"{{"id":"{journal_id}"}}"#),
        )
        .expect("journal should save");
    store
        .enable_journal_sync(journal_id)
        .expect("sync info should save");
    store
        .replace_journal_metadata_diagnostics(journal_id, &[("name", "plaintext")])
        .expect("journal metadata diagnostic should save");
    store
        .upsert_entry_json_row(
            entry_id,
            journal_id,
            None,
            None,
            &format!(r#"{{"id":"{entry_id}","journal_id":"{journal_id}"}}"#),
        )
        .expect("entry should save");
    store
        .upsert_comment_json_row(
            &json!({
                "id": comment_id,
                "journal_id": journal_id,
                "entry_id": entry_id,
                "content": "comment"
            })
            .to_string(),
        )
        .expect("comment should save");
    store
        .upsert_comment_reaction_json_row(
            &json!({
                "id": format!("reaction-{comment_id}"),
                "journal_id": journal_id,
                "entry_id": entry_id,
                "comment_id": comment_id,
                "user_id": "u1",
                "reaction": "like"
            })
            .to_string(),
        )
        .expect("reaction should save");
    store
        .upsert_entry_attachment(&EntryAttachmentUpsert {
            journal_id: journal_id.to_owned(),
            entry_id: entry_id.to_owned(),
            moment_id: moment_id.to_owned(),
            file_path: format!("/tmp/{moment_id}.jpg"),
            moment_type: "image".to_owned(),
            content_type: "image/jpeg".to_owned(),
            file_size_bytes: 123,
            md5_body: format!("md5-{moment_id}"),
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
        .expect("attachment should save");
    store
        .upsert_entry_embedding(
            entry_id,
            journal_id,
            &format!("hash-{entry_id}"),
            &vec![0.0; ENTRY_EMBEDDING_DIMENSION],
        )
        .expect("embedding should save");
    let conn = store.connect().expect("connection should open");
    conn.execute(
        r#"INSERT INTO unread_entries (id, journal_id, entry_id, data_json) VALUES (?1, ?2, ?3, ?4)"#,
        params![
            format!("unread-{entry_id}"),
            journal_id,
            entry_id,
            format!(r#"{{"id":"unread-{entry_id}"}}"#)
        ],
    )
    .expect("unread row should save");
}

#[test]
fn list_entries_orders_by_date_desc() {
    let path = test_store_path("entries-order");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-1";

    store
        .upsert_entry_json_row(
            "entry-z-older",
            journal_id,
            Some("2026-03-01T00:00:00Z"),
            None,
            r#"{"id":"entry-z-older","date":1000}"#,
        )
        .expect("old entry should save");
    store
        .upsert_entry_json_row(
            "entry-a-newer",
            journal_id,
            Some("2026-03-02T00:00:00Z"),
            None,
            r#"{"id":"entry-a-newer","date":2000}"#,
        )
        .expect("new entry should save");

    let rows = store
        .list_entries_json_rows_by_journal_id(
            journal_id,
            None,
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
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
        vec!["entry-a-newer".to_owned(), "entry-z-older".to_owned()]
    );
}

#[test]
fn list_entries_orders_iso_entry_dates_by_timestamp_desc() {
    let path = test_store_path("entries-order-iso-date-desc");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-iso-order";

    store
        .upsert_entry_json_row(
            "entry-z-earlier",
            journal_id,
            Some("2026-03-01T00:00:00Z"),
            None,
            r#"{"id":"entry-z-earlier","date":"2026-03-01T00:00:00.000Z"}"#,
        )
        .expect("earlier entry should save");
    store
        .upsert_entry_json_row(
            "entry-a-later",
            journal_id,
            Some("2026-03-02T00:00:00Z"),
            None,
            r#"{"id":"entry-a-later","date":"2026-03-02T00:00:00.000Z"}"#,
        )
        .expect("later entry should save");

    let rows = store
        .list_entries_json_rows_by_journal_id(
            journal_id,
            None,
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
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
        vec!["entry-a-later".to_owned(), "entry-z-earlier".to_owned()]
    );
}

#[test]
fn list_entries_orders_payload_entry_dates_without_updated_at() {
    let path = test_store_path("entries-order-payload-date-no-updated-at");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-payload-order";

    store
        .upsert_entry_json_row(
            "entry-z-earlier",
            journal_id,
            None,
            None,
            r#"{"id":"entry-z-earlier","payload":{"date":"2026-03-01T00:00:00.000Z"}}"#,
        )
        .expect("earlier entry should save");
    store
        .upsert_entry_json_row(
            "entry-a-later",
            journal_id,
            None,
            None,
            r#"{"id":"entry-a-later","payload":{"date":"2026-03-02T00:00:00.000Z"}}"#,
        )
        .expect("later entry should save");

    let rows = store
        .list_entries_json_rows_by_journal_id(
            journal_id,
            None,
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
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
        vec!["entry-a-later".to_owned(), "entry-z-earlier".to_owned()]
    );
}

#[test]
fn list_entries_orders_by_date_asc_when_requested() {
    let path = test_store_path("entries-order-asc");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-1";

    store
        .upsert_entry_json_row(
            "entry-z-older",
            journal_id,
            Some("2026-03-01T00:00:00Z"),
            None,
            r#"{"id":"entry-z-older","date":1000}"#,
        )
        .expect("old entry should save");
    store
        .upsert_entry_json_row(
            "entry-a-newer",
            journal_id,
            Some("2026-03-02T00:00:00Z"),
            None,
            r#"{"id":"entry-a-newer","date":2000}"#,
        )
        .expect("new entry should save");

    let rows = store
        .list_entries_json_rows_by_journal_id(
            journal_id,
            None,
            EntryListSort::Asc,
            EntryListSortMethod::EntryDate,
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
        vec!["entry-z-older".to_owned(), "entry-a-newer".to_owned()]
    );
}

#[test]
fn entry_upsert_persists_unknown_fields_in_extra_fields_json() {
    let path = test_store_path("entries-extra-fields");
    let store = Store::open_at(&path).expect("store should open");
    store
        .upsert_entry_json_row(
            "entry-extra",
            "journal-extra",
            Some("2026-03-02T00:00:00Z"),
            None,
            r#"{
                "id":"entry-extra",
                "body":"hello",
                "payload":{
                    "body":"hello",
                    "futureClientField":{"nested":true},
                    "weather":"sunny"
                }
            }"#,
        )
        .expect("entry should save");

    let conn = Connection::open(&path).expect("raw connection should open");
    let extra_fields_json: Option<String> = conn
        .query_row(
            "SELECT extra_fields_json FROM entries WHERE id = ?1",
            ["entry-extra"],
            |row| row.get(0),
        )
        .expect("extra_fields_json should query");
    let extra_fields: Value = serde_json::from_str(
        extra_fields_json
            .as_deref()
            .expect("extra_fields_json should exist"),
    )
    .expect("extra_fields_json should be valid JSON");
    assert_eq!(
        extra_fields["futureClientField"]["nested"],
        Value::Bool(true)
    );
    assert_eq!(extra_fields["weather"], "sunny");
    assert!(extra_fields.get("body").is_none());
}

#[test]
fn list_entries_paged_supports_offset() {
    let path = test_store_path("entries-offset");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-offset";
    for (id, updated_at, date_ms) in [
        ("entry-1", "2026-03-01T00:00:00.000Z", 1_000_i64),
        ("entry-2", "2026-03-02T00:00:00.000Z", 2_000_i64),
        ("entry-3", "2026-03-03T00:00:00.000Z", 3_000_i64),
    ] {
        store
            .upsert_entry_json_row(
                id,
                journal_id,
                Some(updated_at),
                None,
                &format!(r#"{{"id":"{id}","date":{date_ms}}}"#),
            )
            .expect("entry should save");
    }

    let page = ListPageRequest {
        limit: Some(1),
        offset: Some(1),
        cursor: None,
    };
    let result = store
        .list_entries_json_rows_by_journal_id_paged(
            journal_id,
            &page,
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
        )
        .expect("paged entries should list");
    assert_eq!(result.rows.len(), 1);
    let id = serde_json::from_str::<Value>(&result.rows[0])
        .expect("row should be json")
        .get("id")
        .and_then(Value::as_str)
        .expect("row should contain id")
        .to_owned();
    assert_eq!(id, "entry-2");
    assert!(result.has_more);
    assert!(result.next_cursor.is_some());
}

#[test]
fn list_entries_paged_supports_cursor() {
    let path = test_store_path("entries-cursor");
    let store = Store::open_at(&path).expect("store should open");
    let journal_id = "j-cursor";
    for (id, updated_at, date_ms) in [
        ("entry-1", "2026-03-01T00:00:00.000Z", 1_000_i64),
        ("entry-2", "2026-03-02T00:00:00.000Z", 2_000_i64),
        ("entry-3", "2026-03-03T00:00:00.000Z", 3_000_i64),
    ] {
        store
            .upsert_entry_json_row(
                id,
                journal_id,
                Some(updated_at),
                None,
                &format!(r#"{{"id":"{id}","date":{date_ms}}}"#),
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
    assert_eq!(first_id, "entry-3");
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
    assert_eq!(second_id, "entry-2");
}

#[test]
fn remap_journal_id_rekeys_tables_embeddings_and_outbox() {
    let path = test_store_path("remap-journal-id-full");
    let store = Store::open_at(&path).expect("store should open");
    let from = "pending-x";
    let to = "105";
    let emb = format!("[{}]", vec!["0.0"; 384].join(","));

    {
        let conn = store.connect().expect("connection should open");
        // A normal journal_id-keyed table (auto-discovered).
        conn.execute(
            "INSERT INTO unread_entries (id, journal_id, entry_id, data_json) VALUES ('u1', ?1, 'e1', '{}')",
            params![from],
        )
        .unwrap();
        // Excluded from the sweep — must stay on the pending id.
        conn.execute(
            "INSERT INTO journal_sync_info (journal_id, status, has_completed) VALUES (?1, 'IDLE', 0)",
            params![from],
        )
        .unwrap();
        // Embeddings: real table is swept; vec (virtual) is rebuilt.
        conn.execute(
            "INSERT INTO entry_embeddings (entry_id, journal_id, source_hash, embedding_json, dimension) VALUES ('e1', ?1, 'h', ?2, 384)",
            params![from, emb],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entry_embeddings_vec (entry_id, journal_id, embedding) VALUES ('e1', ?1, ?2)",
            params![from, emb],
        )
        .unwrap();
    }

    // Outbox: a journal item (object_id == journal id, payload `id`) and
    // entry/media items (payload `journal_id`, object_id `{journal}:...`).
    store
        .enqueue_outbox_item(
            "journal:update:pending-x",
            "journal",
            "update",
            from,
            r#"{"id":"pending-x","name":"DH"}"#,
            0,
        )
        .unwrap();
    store
        .enqueue_outbox_item(
            "entry:pending-x:e1",
            "entry",
            "update",
            "pending-x:e1",
            r#"{"journal_id":"pending-x","entry_id":"e1"}"#,
            0,
        )
        .unwrap();
    store
        .enqueue_outbox_item(
            "media:pending-x:e1:m1",
            "original_media",
            "create",
            "pending-x:e1:m1",
            r#"{"journal_id":"pending-x","entry_id":"e1","moment_id":"m1"}"#,
            0,
        )
        .unwrap();

    let summary = store
        .remap_journal_id(from, to)
        .expect("remap should succeed");

    let conn = store.connect().expect("connection should open");
    let scalar = |sql: &str| -> String {
        conn.query_row(sql, [], |row| row.get::<_, String>(0))
            .unwrap()
    };

    // Auto-discovered table moved.
    assert_eq!(
        scalar("SELECT journal_id FROM unread_entries WHERE id = 'u1'"),
        to
    );
    // Embeddings real table moved + vec rebuilt onto the new id.
    assert_eq!(
        scalar("SELECT journal_id FROM entry_embeddings WHERE entry_id = 'e1'"),
        to
    );
    assert_eq!(
        scalar("SELECT journal_id FROM entry_embeddings_vec WHERE entry_id = 'e1'"),
        to
    );
    assert_eq!(summary.embeddings_rebuilt, 1);

    // Excluded table untouched.
    assert_eq!(
        scalar("SELECT journal_id FROM journal_sync_info LIMIT 1"),
        from,
        "journal_sync_info is managed separately and must not be swept"
    );

    // Outbox items rewritten (object_id + payload).
    let read = |id: &str| -> (String, String) {
        conn.query_row(
            "SELECT object_id, payload_json FROM sync_outbox WHERE id = ?1",
            params![id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap()
    };
    let (j_obj, j_payload) = read("journal:update:pending-x");
    assert_eq!(j_obj, "105");
    assert_eq!(
        serde_json::from_str::<Value>(&j_payload)
            .unwrap()
            .get("id")
            .and_then(Value::as_str),
        Some("105")
    );
    let (e_obj, e_payload) = read("entry:pending-x:e1");
    assert_eq!(e_obj, "105:e1");
    assert_eq!(
        serde_json::from_str::<Value>(&e_payload)
            .unwrap()
            .get("journal_id")
            .and_then(Value::as_str),
        Some("105")
    );
    let (m_obj, m_payload) = read("media:pending-x:e1:m1");
    assert_eq!(m_obj, "105:e1:m1");
    assert_eq!(
        serde_json::from_str::<Value>(&m_payload)
            .unwrap()
            .get("journal_id")
            .and_then(Value::as_str),
        Some("105")
    );
    assert_eq!(summary.outbox_items, 3);
}

#[test]
fn remap_journal_id_leaves_in_flight_create_item_untouched() {
    // The journal's own create item is completed via mark_outbox_item_succeeded,
    // which matches on the lease-time payload. Rewriting it would break that match
    // and re-POST the journal forever, so the remap must leave it alone.
    let path = test_store_path("remap-journal-id-create-item");
    let store = Store::open_at(&path).expect("store should open");
    store
        .enqueue_outbox_item(
            "journal:pending-x",
            "journal",
            "create",
            "pending-x",
            r#"{"id":"pending-x","name":"DH"}"#,
            0,
        )
        .unwrap();

    store.remap_journal_id("pending-x", "105").expect("remap");

    let conn = store.connect().expect("connection should open");
    let (object_id, payload): (String, String) = conn
        .query_row(
            "SELECT object_id, payload_json FROM sync_outbox WHERE id = 'journal:pending-x'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        object_id, "pending-x",
        "create item object_id must be untouched"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&payload)
            .unwrap()
            .get("id")
            .and_then(Value::as_str),
        Some("pending-x"),
        "create item payload id must be untouched"
    );
}

#[test]
fn remap_journal_id_is_idempotent() {
    // A crash between the remap commit and marking the create item done re-runs
    // the remap. It must not error or duplicate (entry_id is the vec PK).
    let path = test_store_path("remap-journal-id-idempotent");
    let store = Store::open_at(&path).expect("store should open");
    let emb = format!("[{}]", vec!["0.0"; 384].join(","));
    {
        let conn = store.connect().expect("connection should open");
        conn.execute(
            "INSERT INTO entry_embeddings (entry_id, journal_id, source_hash, embedding_json, dimension) VALUES ('e1', 'pending-x', 'h', ?1, 384)",
            params![emb],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entry_embeddings_vec (entry_id, journal_id, embedding) VALUES ('e1', 'pending-x', ?1)",
            params![emb],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO unread_entries (id, journal_id, entry_id, data_json) VALUES ('u1', 'pending-x', 'e1', '{}')",
            [],
        )
        .unwrap();
    }

    store
        .remap_journal_id("pending-x", "105")
        .expect("first remap");
    // Second run (e.g. retried create push) must succeed and not duplicate.
    store
        .remap_journal_id("pending-x", "105")
        .expect("second remap is idempotent");

    let conn = store.connect().expect("connection should open");
    let vec_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entry_embeddings_vec WHERE journal_id = '105'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(vec_rows, 1, "vec row must exist exactly once after re-run");
    let unread: String = conn
        .query_row(
            "SELECT journal_id FROM unread_entries WHERE id = 'u1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unread, "105");
}
