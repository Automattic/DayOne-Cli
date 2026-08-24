use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::{EntryId, JournalId};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct CommentReaction {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "journal_id", alias = "journalId", default)]
    pub journal_id: JournalId,
    #[serde(rename = "entry_id", alias = "entryId", default)]
    pub entry_id: EntryId,
    #[serde(rename = "comment_id", alias = "commentId", default)]
    pub comment_id: String,
    #[serde(rename = "user_id", alias = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub reaction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(flatten, default)]
    pub extra: HashMap<String, Value>,
}
