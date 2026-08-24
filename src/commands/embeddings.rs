use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::entry_embeddings::{
    compute_embeddings_for_searchable_texts, entry_searchable_text, source_hash_for_searchable_text,
};
use crate::store::sqlite::Store;

const RECALCULATE_ENTRY_EMBEDDING_BATCH_SIZE: usize = 32;

#[derive(Debug, Clone)]
pub struct RecalculateEntriesArgs {
    pub journal_id: Option<String>,
    pub entry_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecalculateEntriesOutput {
    pub ok: bool,
    pub journal_id: Option<String>,
    pub entry_id: Option<String>,
    pub scanned: usize,
    pub recalculated: usize,
    pub skipped: usize,
}

pub fn recalculate_entries(
    store: &Store,
    args: RecalculateEntriesArgs,
) -> Result<RecalculateEntriesOutput> {
    if !crate::entry_embeddings::embeddings_enabled() {
        anyhow::bail!(
            "embeddings are disabled in this build (compiled without the 'embeddings' feature)"
        );
    }
    let _ = store.delete_stale_or_deleted_entry_embeddings()?;
    let rows =
        store.list_entries_with_embeddings(args.journal_id.as_deref(), args.entry_id.as_deref())?;
    let mut scanned = 0usize;
    let mut recalculated = 0usize;
    let mut skipped = 0usize;
    let mut batch = Vec::with_capacity(RECALCULATE_ENTRY_EMBEDDING_BATCH_SIZE);

    for (row_json, _) in rows {
        scanned += 1;
        let value: Value =
            serde_json::from_str(&row_json).context("failed to decode stored entry JSON row")?;
        if is_deleted_entry(&value) {
            if let Some(entry_id) = entry_id_from_value(&value) {
                // Keep embeddings table aligned with searchable (non-deleted) entries.
                store.delete_entry_embedding(&entry_id)?;
            }
            skipped += 1;
            continue;
        }
        let Some(entry_id) = entry_id_from_value(&value) else {
            skipped += 1;
            continue;
        };
        let Some(journal_id) = journal_id_from_value(&value) else {
            skipped += 1;
            continue;
        };

        let searchable_text = entry_searchable_text(&value);
        let Some(source_hash) = source_hash_for_searchable_text(&searchable_text) else {
            // Drop stale embeddings when entry has no usable text payload.
            store.delete_entry_embedding(&entry_id)?;
            skipped += 1;
            continue;
        };

        batch.push(PendingEntryEmbedding {
            entry_id,
            journal_id,
            source_hash,
            searchable_text,
        });
        if batch.len() >= RECALCULATE_ENTRY_EMBEDDING_BATCH_SIZE {
            recalculated += flush_embedding_batch(store, &mut batch)?;
        }
    }
    recalculated += flush_embedding_batch(store, &mut batch)?;

    Ok(RecalculateEntriesOutput {
        ok: true,
        journal_id: args.journal_id,
        entry_id: args.entry_id,
        scanned,
        recalculated,
        skipped,
    })
}

#[derive(Debug)]
struct PendingEntryEmbedding {
    entry_id: String,
    journal_id: String,
    source_hash: String,
    searchable_text: String,
}

fn flush_embedding_batch(store: &Store, batch: &mut Vec<PendingEntryEmbedding>) -> Result<usize> {
    if batch.is_empty() {
        return Ok(0);
    }
    let texts = batch
        .iter()
        .map(|pending| pending.searchable_text.clone())
        .collect::<Vec<_>>();
    let first_entry_id = batch
        .first()
        .map(|pending| pending.entry_id.as_str())
        .unwrap_or("unknown");
    let embeddings = compute_embeddings_for_searchable_texts(&texts).with_context(|| {
        format!(
            "failed to compute embedding batch of {} entries starting at {first_entry_id}",
            batch.len()
        )
    })?;

    let mut recalculated = 0usize;
    for (pending, embedding) in batch.drain(..).zip(embeddings) {
        store.upsert_entry_embedding(
            &pending.entry_id,
            &pending.journal_id,
            &pending.source_hash,
            &embedding,
        )?;
        recalculated += 1;
    }
    Ok(recalculated)
}

fn entry_id_from_value(value: &Value) -> Option<String> {
    value
        .get("id")
        .and_then(non_empty_scalar_string)
        .or_else(|| {
            value
                .get("revision")
                .and_then(|revision| revision.get("entryId"))
                .and_then(non_empty_scalar_string)
        })
}

fn journal_id_from_value(value: &Value) -> Option<String> {
    value
        .get("journal_id")
        .and_then(non_empty_scalar_string)
        .or_else(|| value.get("journalId").and_then(non_empty_scalar_string))
        .or_else(|| {
            value
                .get("revision")
                .and_then(|revision| revision.get("journalId"))
                .and_then(non_empty_scalar_string)
        })
}

fn is_deleted_entry(value: &Value) -> bool {
    value
        .get("deleted_at")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|deleted_at| !deleted_at.is_empty())
}

fn non_empty_scalar_string(value: &Value) -> Option<String> {
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return None,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
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
        std::env::temp_dir().join(format!("dayone-cli-embeddings-{name}-{unique}.db"))
    }

    #[test]
    fn recalculate_entries_defaults_to_all_entries() {
        let path = test_store_path("recalc-all");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-1",
                "journal-1",
                Some("2026-03-17T10:00:00.000Z"),
                None,
                r#"{"id":"entry-1","journal_id":"journal-1","body":"alpha"}"#,
            )
            .expect("entry 1 should save");
        store
            .upsert_entry_json_row(
                "entry-2",
                "journal-2",
                Some("2026-03-17T11:00:00.000Z"),
                None,
                r#"{"id":"entry-2","journal_id":"journal-2","body":"beta"}"#,
            )
            .expect("entry 2 should save");

        let output = recalculate_entries(
            &store,
            RecalculateEntriesArgs {
                journal_id: None,
                entry_id: None,
            },
        )
        .expect("recalc should succeed");

        assert!(output.ok);
        assert_eq!(output.scanned, 2);
        assert_eq!(output.recalculated, 2);
    }

    #[test]
    fn recalculate_entries_batches_embedding_inference() {
        let path = test_store_path("recalc-batches");
        let store = Store::open_at(&path).expect("store should open");
        for idx in 0..40 {
            let entry_id = format!("entry-{idx:02}");
            let body = format!("batch body {idx}");
            let row = serde_json::json!({
                "id": entry_id,
                "journal_id": "journal-1",
                "body": body,
            });
            store
                .upsert_entry_json_row(
                    row["id"].as_str().expect("id should be a string"),
                    "journal-1",
                    Some("2026-03-17T10:00:00.000Z"),
                    None,
                    &row.to_string(),
                )
                .expect("entry should save");
        }

        let (output, batch_sizes) =
            crate::entry_embeddings::with_test_embedding_batch_sizes(|| {
                recalculate_entries(
                    &store,
                    RecalculateEntriesArgs {
                        journal_id: None,
                        entry_id: None,
                    },
                )
                .expect("recalc should succeed")
            });

        assert_eq!(output.scanned, 40);
        assert_eq!(output.recalculated, 40);
        assert_eq!(output.skipped, 0);
        assert_eq!(batch_sizes, vec![32, 8]);
    }

    #[test]
    fn recalculate_entries_honors_entry_filter() {
        let path = test_store_path("recalc-entry-filter");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-a",
                "journal-1",
                Some("2026-03-17T10:00:00.000Z"),
                None,
                r#"{"id":"entry-a","journal_id":"journal-1","body":"alpha"}"#,
            )
            .expect("entry a should save");
        store
            .upsert_entry_json_row(
                "entry-b",
                "journal-1",
                Some("2026-03-17T11:00:00.000Z"),
                None,
                r#"{"id":"entry-b","journal_id":"journal-1","body":"beta"}"#,
            )
            .expect("entry b should save");

        let output = recalculate_entries(
            &store,
            RecalculateEntriesArgs {
                journal_id: None,
                entry_id: Some("entry-b".to_owned()),
            },
        )
        .expect("recalc should succeed");

        assert_eq!(output.scanned, 1);
        assert_eq!(output.recalculated, 1);
    }

    #[test]
    fn recalculate_entries_uses_revision_fallback_ids() {
        let path = test_store_path("recalc-revision-fallback");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-fallback",
                "journal-fallback",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{
                    "revision":{"entryId":"entry-fallback","journalId":"journal-fallback"},
                    "body":"fallback body"
                }"#,
            )
            .expect("entry should save");

        let output = recalculate_entries(
            &store,
            RecalculateEntriesArgs {
                journal_id: None,
                entry_id: None,
            },
        )
        .expect("recalc should succeed");

        assert_eq!(output.scanned, 1);
        assert_eq!(output.recalculated, 1);
        let source_hash = store
            .get_entry_embedding_source_hash("entry-fallback")
            .expect("hash query should work");
        assert!(source_hash.is_some());
    }

    #[test]
    fn recalculate_entries_removes_embeddings_for_deleted_entries() {
        let path = test_store_path("recalc-deleted-cleans-embedding");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-active",
                "journal-1",
                Some("2026-03-17T11:00:00.000Z"),
                None,
                r#"{"id":"entry-active","journal_id":"journal-1","body":"hello"}"#,
            )
            .expect("active entry should save");
        store
            .upsert_entry_json_row(
                "entry-deleted",
                "journal-1",
                Some("2026-03-17T12:00:00.000Z"),
                Some("2026-03-18T12:00:00.000Z"),
                r#"{"id":"entry-deleted","journal_id":"journal-1","deleted_at":"2026-03-18T12:00:00.000Z","body":"bye"}"#,
            )
            .expect("deleted entry should save");
        store
            .upsert_entry_embedding("entry-deleted", "journal-1", "hash", &[0.1, 0.2])
            .expect("embedding should save");

        let output = recalculate_entries(
            &store,
            RecalculateEntriesArgs {
                journal_id: None,
                entry_id: None,
            },
        )
        .expect("recalc should succeed");

        assert_eq!(output.scanned, 1);
        assert_eq!(output.recalculated, 1);
        assert_eq!(output.skipped, 0);
        let source_hash = store
            .get_entry_embedding_source_hash("entry-deleted")
            .expect("hash query should work");
        assert!(source_hash.is_none());
    }
}
