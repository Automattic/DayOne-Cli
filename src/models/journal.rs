use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::JournalId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    #[serde(default)]
    pub id: JournalId,
    #[serde(rename = "updated_at", alias = "updatedAt", default)]
    pub updated_at: Option<String>,
    #[serde(rename = "deleted_at", alias = "deletedAt", default)]
    pub deleted_at: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::Journal;
    use serde_json::{Value, json};

    #[test]
    fn journal_round_trips_unknown_fields() {
        let input = json!({
            "id": "journal-1",
            "updated_at": "2026-03-20T12:00:00.000Z",
            "deleted_at": null,
            "name": "Personal",
            "unknownJournalField": true
        });
        let journal: Journal = serde_json::from_value(input).expect("journal should deserialize");
        assert_eq!(journal.id.as_ref(), "journal-1");
        assert_eq!(
            journal.updated_at.as_deref(),
            Some("2026-03-20T12:00:00.000Z")
        );
        assert_eq!(
            journal.extra.get("unknownJournalField"),
            Some(&Value::Bool(true))
        );

        let serialized = serde_json::to_value(&journal).expect("journal should serialize");
        assert_eq!(
            serialized.get("updated_at").and_then(Value::as_str),
            Some("2026-03-20T12:00:00.000Z")
        );
        assert!(serialized.get("updatedAt").is_none());
        assert_eq!(
            serialized.get("unknownJournalField"),
            Some(&Value::Bool(true))
        );
    }
}
