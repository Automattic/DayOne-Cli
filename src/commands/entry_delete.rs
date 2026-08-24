use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::store::sqlite::Store;
use crate::util::{normalize_entry_id, now_rfc3339_utc};

#[derive(Debug, Clone)]
pub struct EntryDeleteArgs {
    pub journal_id: String,
    pub entry_id: String,
}

#[derive(Debug, Serialize)]
pub struct EntryDeleteOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub journal_id: String,
    pub entry_id: String,
    pub operation: String,
    pub queued: bool,
    pub outbox_id: String,
    pub synced: bool,
}

pub fn execute(store: &Store, base_url: &str, args: EntryDeleteArgs) -> Result<EntryDeleteOutput> {
    let journal_id = args.journal_id.trim().to_owned();
    if journal_id.is_empty() {
        bail!("journal_id must not be empty");
    }
    let entry_id = normalize_entry_id(&args.entry_id);
    if entry_id.is_empty() {
        bail!("entry_id must not be empty");
    }

    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;

    let _journal_row = store
        .get_json_row_by_id("journals", &journal_id)?
        .ok_or_else(|| {
            anyhow!(
                "journal '{}' not found locally; run `dayone sync` first",
                journal_id
            )
        })?;

    let existing_entry = store
        .get_json_row_by_id("entries", &entry_id)?
        .ok_or_else(|| {
            anyhow!(
                "entry '{}' not found locally; run `dayone sync` first",
                entry_id
            )
        })?;
    let existing_entry_journal_id = store
        .get_entry_journal_id(&entry_id)?
        .ok_or_else(|| anyhow!("entry '{}' journal mapping not found locally", entry_id))?;
    if existing_entry_journal_id != journal_id {
        bail!(
            "entry '{}' belongs to journal '{}', not '{}'",
            entry_id,
            existing_entry_journal_id,
            journal_id
        );
    }

    let now_iso = now_rfc3339_utc();
    let now_ms = now_epoch_ms();
    let mut entry_value: Value = serde_json::from_str(&existing_entry)
        .map_err(|e| anyhow!("failed to parse JSON for entry '{}': {}", entry_id, e))?;
    let Some(entry_obj) = entry_value.as_object_mut() else {
        bail!(
            "entry payload for entry '{}' must be a JSON object",
            entry_id
        );
    };
    entry_obj.insert("id".to_owned(), Value::String(entry_id.clone()));
    entry_obj.insert("deleted_at".to_owned(), Value::String(now_iso.clone()));

    store.upsert_entry_json_row_with_edit_date(
        &entry_id,
        &journal_id,
        Some(&now_iso),
        Some(&now_iso),
        Some(&now_iso),
        &entry_value.to_string(),
    )?;
    // A delete should cancel any pending original media uploads for this entry.
    let _ = store.delete_original_media_outbox_for_entry(&journal_id, &entry_id)?;

    let outbox_id = format!("entry:{journal_id}:{entry_id}");
    let outbox_payload = json!({
        "journal_id": &journal_id,
        "entry_id": &entry_id,
        "queued_at": &now_iso,
        "queued_at_epoch_ms": now_ms
    });
    store.enqueue_outbox_item(
        &outbox_id,
        "entry",
        "delete",
        &format!("{journal_id}:{entry_id}"),
        &outbox_payload.to_string(),
        now_ms,
    )?;

    Ok(EntryDeleteOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        journal_id,
        entry_id,
        operation: "delete".to_owned(),
        queued: true,
        outbox_id,
        synced: false,
    })
}

fn now_epoch_ms() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::Store;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_store_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("dayone-cli-entry-delete-{name}-{unique}.db"))
    }

    fn setup_store(base_url: &str) -> Store {
        let path = test_store_path("store");
        let store = Store::open_at(&path).expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url(base_url)
            .expect("profile should create");
        store
            .save_auth_session(
                profile.id,
                "tok",
                "2026-03-17T00:00:00.000Z",
                r#"{"id":"test-user"}"#,
            )
            .expect("session should save");
        store
            .upsert_json_row(
                "journals",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"journal-1","name":"Journal 1"}"#,
            )
            .expect("journal should save");
        store
    }

    #[test]
    fn delete_rejects_missing_entry() {
        let base_url = "https://stg.dayone.me";
        let store = setup_store(base_url);
        let err = execute(
            &store,
            base_url,
            EntryDeleteArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "missing".to_owned(),
            },
        )
        .expect_err("delete should fail when entry missing");
        assert!(err.to_string().contains("not found locally"));
    }

    #[test]
    fn delete_rejects_journal_entry_mismatch() {
        let base_url = "https://stg.dayone.me";
        let store = setup_store(base_url);
        store
            .upsert_json_row(
                "journals",
                "journal-2",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"journal-2","name":"Journal 2"}"#,
            )
            .expect("journal should save");
        store
            .upsert_entry_json_row(
                "entry-1",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"entry-1","body":"hello"}"#,
            )
            .expect("entry should save");

        let err = execute(
            &store,
            base_url,
            EntryDeleteArgs {
                journal_id: "journal-2".to_owned(),
                entry_id: "entry-1".to_owned(),
            },
        )
        .expect_err("delete should fail when journal mismatches");
        assert!(err.to_string().contains("belongs to journal"));
    }

    #[test]
    fn delete_sets_tombstone_and_enqueues_outbox() {
        let base_url = "https://stg.dayone.me";
        let store = setup_store(base_url);
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

        let out = execute(
            &store,
            base_url,
            EntryDeleteArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "entry-1".to_owned(),
            },
        )
        .expect("delete should succeed");
        assert_eq!(out.operation, "delete");
        assert_eq!(out.outbox_id, "entry:journal-1:entry-1");

        let row = store
            .get_json_row_by_id("entries", "entry-1")
            .expect("query should work")
            .expect("entry should exist");
        let value: Value = serde_json::from_str(&row).expect("entry row should parse");
        assert!(value.get("deleted_at").and_then(Value::as_str).is_some());
        let user_edit_date = store
            .get_entry_user_edit_date("entry-1")
            .expect("query should work");
        assert!(user_edit_date.is_some());
        assert_ne!(
            user_edit_date.as_deref(),
            Some("2026-03-17T00:00:00.000Z"),
            "delete should advance persisted user_edit_date"
        );

        let queued = store
            .lease_outbox_items(now_epoch_ms() + 1_000, 10)
            .expect("outbox should list");
        assert!(
            queued
                .iter()
                .any(|item| item.id == "entry:journal-1:entry-1"
                    && item.resource == "entry"
                    && item.operation == "delete")
        );
    }

    #[test]
    fn delete_normalizes_lowercase_hex_entry_id() {
        let base_url = "https://stg.dayone.me";
        let store = setup_store(base_url);
        store
            .upsert_entry_json_row_with_edit_date(
                "A65AFA22B98A0DEC62EAAD3CD1CCF45D",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"A65AFA22B98A0DEC62EAAD3CD1CCF45D","body":"hello"}"#,
            )
            .expect("entry should save");

        let out = execute(
            &store,
            base_url,
            EntryDeleteArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "a65afa22b98a0dec62eaad3cd1ccf45d".to_owned(),
            },
        )
        .expect("delete should succeed");
        assert_eq!(out.entry_id, "A65AFA22B98A0DEC62EAAD3CD1CCF45D");
    }

    #[test]
    fn upsert_rejects_invalid_json() {
        let store = setup_store("https://stg.dayone.me");
        let err = store
            .upsert_entry_json_row_with_edit_date(
                "entry-invalid-json",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                Some("2026-03-17T00:00:00.000Z"),
                None,
                "{not-json",
            )
            .expect_err("upsert should reject invalid JSON");
        assert!(err.to_string().contains("invalid JSON"));
    }

    #[test]
    fn delete_keeps_json_payload_entry_shaped() {
        let base_url = "https://stg.dayone.me";
        let store = setup_store(base_url);
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-shape",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"entry-shape","body":"hello"}"#,
            )
            .expect("entry should save");

        execute(
            &store,
            base_url,
            EntryDeleteArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "entry-shape".to_owned(),
            },
        )
        .expect("delete should succeed");

        let row = store
            .get_json_row_by_id("entries", "entry-shape")
            .expect("query should work")
            .expect("entry should exist");
        let value: Value = serde_json::from_str(&row).expect("entry row should parse");
        assert!(value.get("deleted_at").and_then(Value::as_str).is_some());
        assert!(
            value.get("journal_id").is_none(),
            "json payload should not duplicate row-level journal_id"
        );
        assert!(
            value.get("updated_at").is_none(),
            "json payload should not duplicate row-level updated_at"
        );
    }

    #[test]
    fn delete_cancels_pending_original_media_outbox_items_for_entry() {
        let base_url = "https://stg.dayone.me";
        let store = setup_store(base_url);
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-media",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"entry-media","body":"hello"}"#,
            )
            .expect("entry should save");
        store
            .enqueue_outbox_item(
                "original_media:journal-1:entry-media:m1",
                "original_media",
                "create",
                "journal-1:entry-media:m1",
                r#"{"journal_id":"journal-1","entry_id":"entry-media","moment_id":"m1"}"#,
                now_epoch_ms(),
            )
            .expect("original media outbox should save");

        execute(
            &store,
            base_url,
            EntryDeleteArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "entry-media".to_owned(),
            },
        )
        .expect("delete should succeed");

        let leased = store
            .lease_outbox_items(now_epoch_ms() + 1_000, 50)
            .expect("outbox should lease");
        assert!(
            !leased.iter().any(|item| {
                item.resource == "original_media"
                    && item.object_id.starts_with("journal-1:entry-media:")
            }),
            "pending original_media outbox items for deleted entry should be removed"
        );
    }
}
