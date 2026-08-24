use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;

use crate::store::entries::json::rebuild_entry_content;
use crate::store::sqlite::Store;
use crate::util::normalize_entry_id;

#[derive(Debug, Clone)]
pub struct EntryReadArgs {
    pub journal_id: String,
    pub entry_id: String,
}

#[derive(Debug, Serialize)]
pub struct EntryReadOutput {
    pub ok: bool,
    pub journal_id: String,
    pub entry_id: String,
    pub updated_at: Option<String>,
    pub user_edit_date: Option<String>,
    /// The reconstructed entry content: known fields (date, title, body,
    /// richTextJSON, timeZone, tags, moments, clientMeta) plus any preserved
    /// passthrough fields (location, weather, ...), matching the shape
    /// synced to the API.
    pub entry: Value,
}

/// Local single-entry fetch from the SQLite store. No network access.
pub fn execute(store: &Store, args: EntryReadArgs) -> Result<EntryReadOutput> {
    let journal_id = args.journal_id.trim().to_owned();
    if journal_id.is_empty() {
        bail!("journal_id must not be empty");
    }
    let entry_id = normalize_entry_id(&args.entry_id);
    if entry_id.is_empty() {
        bail!("entry_id must not be empty");
    }

    let record = store.get_entry_row_record(&entry_id)?.ok_or_else(|| {
        anyhow!(
            "entry '{}' not found locally; run `dayone sync` first",
            entry_id
        )
    })?;
    if record.journal_id != journal_id {
        bail!(
            "entry '{}' belongs to journal '{}', not '{}'",
            entry_id,
            record.journal_id,
            journal_id
        );
    }

    let local: Value = serde_json::from_str(&record.data_json)
        .map_err(|e| anyhow!("failed to parse JSON for entry '{}': {}", entry_id, e))?;
    let entry = rebuild_entry_content(&local, record.extra_fields_json.as_deref(), &entry_id);

    Ok(EntryReadOutput {
        ok: true,
        journal_id,
        entry_id,
        updated_at: record.updated_at,
        user_edit_date: record.user_edit_date,
        entry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::Store;

    fn setup_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let store = Store::open_at(dir.path().join("dayone.db")).expect("store should open");
        (dir, store)
    }

    #[test]
    fn read_returns_full_reconstructed_entry() {
        let (_dir, store) = setup_store();
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-1",
                "journal-1",
                Some("1742169600000"),
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{
                    "id":"entry-1",
                    "date":"2026-03-17T10:00:00.000Z",
                    "title":"Captain's log",
                    "body":"USS Voyager\nStardate 41153.7",
                    "tags":["ship","ops"],
                    "moments":[{"id":"m1","momentType":"image"}],
                    "weather":{"conditionCode":"clear"},
                    "location":{"city":"Portland"}
                }"#,
            )
            .expect("entry should save");

        let out = execute(
            &store,
            EntryReadArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "entry-1".to_owned(),
            },
        )
        .expect("read should succeed");

        assert!(out.ok);
        assert_eq!(out.journal_id, "journal-1");
        assert_eq!(out.entry_id, "entry-1");
        assert_eq!(out.updated_at.as_deref(), Some("2026-03-17T00:00:00.000Z"));
        assert_eq!(out.user_edit_date.as_deref(), Some("1742169600000"));
        assert_eq!(out.entry["id"], "entry-1");
        assert_eq!(out.entry["title"], "Captain's log");
        assert_eq!(out.entry["body"], "USS Voyager\nStardate 41153.7");
        assert_eq!(out.entry["tags"], serde_json::json!(["ship", "ops"]));
        assert_eq!(out.entry["moments"][0]["id"], "m1");
        // Passthrough fields preserved via extra_fields_json.
        assert_eq!(out.entry["weather"]["conditionCode"], "clear");
        assert_eq!(out.entry["location"]["city"], "Portland");
    }

    #[test]
    fn read_rejects_missing_entry() {
        let (_dir, store) = setup_store();
        let err = execute(
            &store,
            EntryReadArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "missing".to_owned(),
            },
        )
        .expect_err("read should fail when entry missing");
        assert!(err.to_string().contains("not found locally"));
    }

    #[test]
    fn read_rejects_journal_entry_mismatch() {
        let (_dir, store) = setup_store();
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
            EntryReadArgs {
                journal_id: "journal-2".to_owned(),
                entry_id: "entry-1".to_owned(),
            },
        )
        .expect_err("read should fail when journal mismatches");
        assert!(err.to_string().contains("belongs to journal"));
    }

    #[test]
    fn read_normalizes_lowercase_hex_entry_id() {
        let (_dir, store) = setup_store();
        store
            .upsert_entry_json_row(
                "A65AFA22B98A0DEC62EAAD3CD1CCF45D",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"A65AFA22B98A0DEC62EAAD3CD1CCF45D","body":"hello"}"#,
            )
            .expect("entry should save");

        let out = execute(
            &store,
            EntryReadArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "a65afa22b98a0dec62eaad3cd1ccf45d".to_owned(),
            },
        )
        .expect("read should succeed");
        assert_eq!(out.entry_id, "A65AFA22B98A0DEC62EAAD3CD1CCF45D");
    }
}
