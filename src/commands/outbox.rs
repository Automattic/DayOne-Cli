//! Local sync-outbox inspection and recovery commands.
//!
//! These are offline commands: they operate directly on the profile's SQLite
//! database and never touch the network, so users can inspect and unblock a
//! stuck sync queue even when pushes keep failing.

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::store::OutboxItemDetails;
use crate::store::sqlite::Store;

#[derive(Debug, Serialize)]
pub struct OutboxListOutput {
    pub ok: bool,
    pub base_url: String,
    pub count: usize,
    pub items: Vec<OutboxListItem>,
}

#[derive(Debug, Serialize)]
pub struct OutboxListItem {
    #[serde(flatten)]
    pub details: OutboxItemDetails,
    /// Payload parsed as JSON when possible, for readability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

pub fn list(store: &Store, base_url: &str, include_payload: bool) -> Result<OutboxListOutput> {
    let items = store
        .list_outbox_item_details(include_payload)?
        .into_iter()
        .map(|details| {
            // Fall back to the raw string for malformed payloads: this is an
            // inspection command, so users must still be able to see (and
            // decide to clear) exactly the rows that are broken.
            let payload = details.payload_json.as_deref().map(|raw| {
                serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
            });
            OutboxListItem { details, payload }
        })
        .collect::<Vec<_>>();
    Ok(OutboxListOutput {
        ok: true,
        base_url: base_url.to_owned(),
        count: items.len(),
        items,
    })
}

#[derive(Debug, Serialize)]
pub struct OutboxClearOutput {
    pub ok: bool,
    pub base_url: String,
    pub removed: usize,
}

#[derive(Debug, Clone)]
pub struct OutboxClearArgs {
    /// Remove only this item id.
    pub id: Option<String>,
    /// Remove only dead-lettered (`failed`) items.
    pub failed_only: bool,
    /// Remove everything, including pending items (requires explicit flag).
    pub all: bool,
}

pub fn clear(store: &Store, base_url: &str, args: OutboxClearArgs) -> Result<OutboxClearOutput> {
    let removed = match (&args.id, args.failed_only, args.all) {
        (Some(id), false, false) => store.delete_outbox_item_by_id(id)?,
        (None, true, false) => store.delete_outbox_items_by_status("failed")?,
        (None, false, true) => store.delete_all_outbox_items()?,
        (None, false, false) => {
            bail!("specify what to clear: --id <ID>, --failed, or --all")
        }
        _ => bail!("--id, --failed, and --all are mutually exclusive"),
    };
    Ok(OutboxClearOutput {
        ok: true,
        base_url: base_url.to_owned(),
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn setup_store() -> Store {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("dayone-cli-outbox-cmd-{unique}.db"));
        let store = Store::open_at(&path).expect("store should open");
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-1",
                "entry",
                "update",
                "journal-1:entry-1",
                r#"{"journal_id":"journal-1","entry_id":"entry-1"}"#,
                0,
            )
            .expect("pending item should save");
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-2",
                "entry",
                "update",
                "journal-1:entry-2",
                r#"{"journal_id":"journal-1","entry_id":"entry-2"}"#,
                0,
            )
            .expect("second item should save");
        // Dead-letter the second item through the real state machine.
        let conn = store.connect().expect("connection should open");
        conn.execute(
            "UPDATE sync_outbox SET status = 'processing' WHERE id = 'entry:journal-1:entry-2'",
            [],
        )
        .expect("lease should apply");
        store
            .mark_outbox_item_failed(
                "entry:journal-1:entry-2",
                r#"{"journal_id":"journal-1","entry_id":"entry-2"}"#,
                8,
                Some("failed to deserialize rebuilt local entry content during outbox push"),
            )
            .expect("dead-letter should apply");
        store
    }

    #[test]
    fn list_reports_status_attempts_and_last_error() {
        let store = setup_store();
        let output = list(&store, "https://stg.dayone.me", false).expect("list should succeed");
        assert_eq!(output.count, 2);
        let failed = output
            .items
            .iter()
            .find(|item| item.details.id == "entry:journal-1:entry-2")
            .expect("failed item should be listed");
        assert_eq!(failed.details.status, "failed");
        assert_eq!(failed.details.attempt_count, 8);
        assert!(
            failed
                .details
                .last_error
                .as_deref()
                .is_some_and(|err| err.contains("failed to deserialize")),
            "last_error should surface the push failure"
        );
        assert!(failed.payload.is_none(), "payload is opt-in");
    }

    #[test]
    fn list_includes_parsed_payload_when_requested() {
        let store = setup_store();
        let output = list(&store, "https://stg.dayone.me", true).expect("list should succeed");
        let item = output.items.first().expect("items should exist");
        assert_eq!(
            item.payload
                .as_ref()
                .and_then(|payload| payload.get("entry_id"))
                .and_then(Value::as_str),
            Some("entry-1")
        );
    }

    #[test]
    fn list_surfaces_malformed_payload_as_raw_string() {
        let store = setup_store();
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-broken",
                "entry",
                "update",
                "journal-1:entry-broken",
                "{not-valid-json",
                0,
            )
            .expect("malformed payload should still store");
        let output = list(&store, "https://stg.dayone.me", true).expect("list should succeed");
        let broken = output
            .items
            .iter()
            .find(|item| item.details.id == "entry:journal-1:entry-broken")
            .expect("broken item should be listed");
        assert_eq!(
            broken.payload,
            Some(Value::String("{not-valid-json".to_owned())),
            "malformed payloads must stay inspectable as raw strings"
        );
    }

    #[test]
    fn clear_failed_removes_only_dead_lettered_items() {
        let store = setup_store();
        let output = clear(
            &store,
            "https://stg.dayone.me",
            OutboxClearArgs {
                id: None,
                failed_only: true,
                all: false,
            },
        )
        .expect("clear should succeed");
        assert_eq!(output.removed, 1);
        let remaining = store
            .list_outbox_item_details(false)
            .expect("list should succeed");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "entry:journal-1:entry-1");
    }

    #[test]
    fn clear_by_id_removes_single_item() {
        let store = setup_store();
        let output = clear(
            &store,
            "https://stg.dayone.me",
            OutboxClearArgs {
                id: Some("entry:journal-1:entry-1".to_owned()),
                failed_only: false,
                all: false,
            },
        )
        .expect("clear should succeed");
        assert_eq!(output.removed, 1);
    }

    #[test]
    fn clear_all_removes_everything() {
        let store = setup_store();
        let output = clear(
            &store,
            "https://stg.dayone.me",
            OutboxClearArgs {
                id: None,
                failed_only: false,
                all: true,
            },
        )
        .expect("clear should succeed");
        assert_eq!(output.removed, 2);
    }

    #[test]
    fn clear_requires_an_explicit_selector() {
        let store = setup_store();
        let err = clear(
            &store,
            "https://stg.dayone.me",
            OutboxClearArgs {
                id: None,
                failed_only: false,
                all: false,
            },
        )
        .expect_err("clear without selector should fail");
        assert!(err.to_string().contains("specify what to clear"));
    }
}
