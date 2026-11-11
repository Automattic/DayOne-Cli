//! Inspect stored source hashes without model initialization or database writes.

use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

use crate::entry_embeddings::{entry_searchable_text, source_hash_for_searchable_text};

use super::{CheckOutcome, table_exists};

#[derive(Default, Serialize)]
struct IndexDetails {
    enabled: bool,
    eligible_entries: usize,
    missing_embeddings: usize,
    stale_embeddings: usize,
    current_embeddings: usize,
    unreadable_entries: usize,
    inspection_complete: bool,
    model_runtime_checked: bool,
}

pub(super) fn run(connection: Option<&Connection>) -> CheckOutcome {
    if !cfg!(feature = "embeddings") {
        return CheckOutcome::pass(
            "embedding_index",
            "Embeddings are disabled in this build",
            serde_json::json!({
                "enabled": false,
                "reason": "feature_disabled",
                "model_runtime_checked": false,
            }),
        );
    }
    let Some(connection) = connection else {
        return CheckOutcome::not_run(
            "embedding_index",
            "Cannot inspect embedding source hashes",
            "sqlite_unavailable",
        );
    };
    if !table_exists(connection, "entries") || !table_exists(connection, "entry_embeddings") {
        return CheckOutcome::not_run(
            "embedding_index",
            "Cannot inspect embedding source hashes",
            "schema_unavailable",
        );
    }
    let Ok(details) = inspect(connection) else {
        return CheckOutcome::not_run(
            "embedding_index",
            "Cannot inspect embedding source hashes",
            "sqlite_query_failed",
        );
    };
    let needs_recalculation = details.missing_embeddings > 0 || details.stale_embeddings > 0;
    let mut outcome = if details.unreadable_entries > 0 {
        CheckOutcome::fail(
            "embedding_index",
            "Some live entries contain unreadable data",
            details,
        )
        .with_finding(
            "embedding_entry_data_unreadable",
            "Cannot verify embedding source hashes for unreadable entries",
            "Resolve the unreadable local entry data. Then run dayone doctor again",
        )
    } else if needs_recalculation {
        CheckOutcome::finding(
            "embedding_index",
            "Some live text entries have missing or stale embeddings",
            details,
        )
    } else {
        CheckOutcome::pass(
            "embedding_index",
            "Embedding source hashes are current for live text entries. This check does not test the model runtime",
            details,
        )
    };
    if needs_recalculation {
        outcome = outcome.with_finding(
            "embedding_index_incomplete",
            "Semantic search can omit entries or use outdated text",
            "Run dayone embeddings recalculate-entries",
        );
    }
    outcome
}

fn inspect(connection: &Connection) -> rusqlite::Result<IndexDetails> {
    let mut details = IndexDetails {
        enabled: true,
        ..IndexDetails::default()
    };
    let mut statement = connection.prepare(
        "SELECT e.data_json, ee.source_hash
         FROM entries e
         LEFT JOIN entry_embeddings ee ON ee.entry_id = e.id
         WHERE e.is_deleted = 0",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let entry = match serde_json::from_str::<Value>(&raw) {
            Ok(entry) if entry.is_object() => entry,
            _ => {
                details.unreadable_entries += 1;
                continue;
            }
        };
        let text = entry_searchable_text(&entry);
        let Some(expected_hash) = source_hash_for_searchable_text(&text) else {
            continue;
        };
        details.eligible_entries += 1;
        match row.get::<_, Option<String>>(1)? {
            None => details.missing_embeddings += 1,
            Some(hash) if hash == expected_hash => details.current_embeddings += 1,
            Some(_) => details.stale_embeddings += 1,
        }
    }
    details.inspection_complete = details.unreadable_entries == 0;
    Ok(details)
}
