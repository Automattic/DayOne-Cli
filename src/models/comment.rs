use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::{EntryId, JournalId};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Comment {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "journal_id", alias = "journalId", default)]
    pub journal_id: JournalId,
    #[serde(rename = "entry_id", alias = "entryId", default)]
    pub entry_id: EntryId,
    #[serde(rename = "author_id", alias = "authorId", default)]
    pub author_id: String,
    #[serde(
        rename = "created_at",
        alias = "createdAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<String>,
    #[serde(
        rename = "updated_at",
        alias = "updatedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_at: Option<String>,
    #[serde(
        rename = "deleted_at",
        alias = "deletedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(flatten, default)]
    pub extra: HashMap<String, Value>,
}
