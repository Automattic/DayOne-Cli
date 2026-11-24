use serde::Serialize;
use serde_json::Value;

use crate::entry_embeddings::{
    compute_embedding_for_searchable_text, entry_searchable_text, source_hash_for_searchable_text,
};
use crate::store::sqlite::Store;

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingSyncStatus {
    #[default]
    NotRun,
    Disabled,
    Success,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingSyncError {
    GenerationFailed,
    StorageFailed,
}

#[derive(Debug, Default, Serialize)]
pub struct EmbeddingSyncOutput {
    pub status: EmbeddingSyncStatus,
    pub generated: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub skipped: usize,
    pub error: Option<EmbeddingSyncError>,
}

impl EmbeddingSyncOutput {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            status: if enabled {
                EmbeddingSyncStatus::Success
            } else {
                EmbeddingSyncStatus::Disabled
            },
            ..Self::default()
        }
    }

    pub(crate) fn update_entry(
        &mut self,
        store: &Store,
        entry_id: &str,
        journal_id: &str,
        entry: &Value,
    ) {
        if matches!(
            self.status,
            EmbeddingSyncStatus::Disabled | EmbeddingSyncStatus::NotRun
        ) {
            return;
        }
        let text = entry_searchable_text(entry);
        let Some(hash) = source_hash_for_searchable_text(&text) else {
            return;
        };
        match store.get_entry_embedding_source_hash(entry_id) {
            Ok(Some(existing)) if existing == hash => {
                self.unchanged += 1;
                return;
            }
            Ok(_) => {}
            Err(err) => {
                self.record_failure(EmbeddingSyncError::StorageFailed);
                super::engine::log_sync(format!("entry embedding hash lookup failed: {err}"));
                return;
            }
        }
        if self.status == EmbeddingSyncStatus::Failed {
            self.skipped += 1;
            return;
        }
        match compute_embedding_for_searchable_text(&text) {
            Ok(Some(embedding)) => {
                match store.upsert_entry_embedding(entry_id, journal_id, &hash, &embedding) {
                    Ok(()) => self.generated += 1,
                    Err(err) => {
                        self.record_failure(EmbeddingSyncError::StorageFailed);
                        super::engine::log_sync(format!("entry embedding upsert failed: {err}"));
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                self.record_failure(EmbeddingSyncError::GenerationFailed);
                super::engine::log_sync(format!(
                    "entry embedding generation failed: {err}; disabling embedding generation for the remaining entries in this sync run"
                ));
            }
        }
    }

    fn record_failure(&mut self, error: EmbeddingSyncError) {
        self.status = EmbeddingSyncStatus::Failed;
        self.failed += 1;
        self.error.get_or_insert(error);
    }
}
