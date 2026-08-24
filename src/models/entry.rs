use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
use serde_json::Value;

use crate::models::{EntryId, JournalId, Moment};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(default)]
    pub id: EntryId,
    #[serde(
        rename = "journal_id",
        alias = "journalId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub journal_id: Option<JournalId>,
    #[serde(
        rename = "updated_at",
        alias = "updatedAt",
        default,
        deserialize_with = "deserialize_optional_string_or_number",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_at: Option<String>,
    #[serde(
        rename = "deleted_at",
        alias = "deletedAt",
        default,
        deserialize_with = "deserialize_optional_string_or_number",
        skip_serializing_if = "Option::is_none"
    )]
    pub deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Value>,
    #[serde(
        rename = "user_edit_date",
        alias = "userEditDate",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub user_edit_date: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(
        rename = "richTextJSON",
        alias = "richTextJson",
        alias = "rich_text_json",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub rich_text_json: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(
        default,
        deserialize_with = "deserialize_null_as_empty_moments",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub moments: Vec<Moment>,
    #[serde(flatten, default)]
    pub extra: HashMap<String, Value>,
}

/// Some clients emit `"moments": null` where the CLI expects an array;
/// serde's `default` only covers a missing key, not an explicit null.
fn deserialize_null_as_empty_moments<'de, D>(deserializer: D) -> Result<Vec<Moment>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Vec<Moment>>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

fn deserialize_optional_string_or_number<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => Err(D::Error::custom(format!(
            "expected string, number, or null, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::Entry;
    use crate::models::{EntryId, JournalId};
    use serde_json::{Value, json};
    use std::collections::HashMap;

    #[test]
    fn entry_round_trips_unknown_fields_and_moments() {
        let input = json!({
            "id": "entry-1",
            "journal_id": "journal-1",
            "updated_at": "2026-03-20T12:00:00.000Z",
            "deleted_at": null,
            "body": "hello",
            "moments": [{
                "id": "m1",
                "type": "image",
                "contentType": "image/jpeg",
                "fileSize": 42,
                "customMomentField": "kept"
            }],
            "unknownTopLevel": {"k": "v"}
        });
        let entry: Entry = serde_json::from_value(input).expect("entry should deserialize");
        assert_eq!(entry.id, EntryId::from("entry-1"));
        assert_eq!(
            entry.journal_id.as_ref(),
            Some(&JournalId::from("journal-1"))
        );
        assert_eq!(entry.moments.len(), 1);
        assert_eq!(
            entry.moments[0].extra.get("customMomentField"),
            Some(&Value::String("kept".to_owned()))
        );
        assert_eq!(entry.extra.get("unknownTopLevel"), Some(&json!({"k": "v"})));

        let serialized = serde_json::to_value(&entry).expect("entry should serialize");
        assert_eq!(serialized.get("unknownTopLevel"), Some(&json!({"k": "v"})));
        assert_eq!(
            serialized
                .get("moments")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("customMomentField")),
            Some(&Value::String("kept".to_owned()))
        );
    }

    #[test]
    fn entry_serializes_legacy_snake_case_and_rich_text_json_key() {
        let entry = Entry {
            id: EntryId::from("entry-2"),
            journal_id: Some(JournalId::from("journal-2")),
            updated_at: Some("2026-03-31T00:00:00.000Z".to_owned()),
            deleted_at: Some("2026-03-31T01:00:00.000Z".to_owned()),
            cursor: None,
            date: None,
            user_edit_date: Some(Value::String("2026-03-31T00:30:00.000Z".to_owned())),
            body: Some("body".to_owned()),
            rich_text_json: Some(Value::String("{\"ops\":[]}".to_owned())),
            payload: None,
            moments: Vec::new(),
            extra: HashMap::new(),
        };

        let serialized = serde_json::to_value(&entry).expect("entry should serialize");
        assert_eq!(
            serialized.get("journal_id").and_then(Value::as_str),
            Some("journal-2")
        );
        assert_eq!(
            serialized.get("updated_at").and_then(Value::as_str),
            Some("2026-03-31T00:00:00.000Z")
        );
        assert_eq!(
            serialized.get("deleted_at").and_then(Value::as_str),
            Some("2026-03-31T01:00:00.000Z")
        );
        assert!(serialized.get("journalId").is_none());
        assert!(serialized.get("updatedAt").is_none());
        assert!(serialized.get("deletedAt").is_none());
        assert_eq!(
            serialized.get("richTextJSON").and_then(Value::as_str),
            Some("{\"ops\":[]}")
        );
        assert!(serialized.get("richTextJson").is_none());
    }

    #[test]
    fn entry_accepts_string_date_values() {
        let input = json!({
            "id": "entry-3",
            "date": "2026-03-31T00:00:00.000Z"
        });
        let entry: Entry = serde_json::from_value(input).expect("entry should deserialize");
        assert_eq!(entry.id, EntryId::from("entry-3"));
        assert_eq!(
            entry.date,
            Some(Value::String("2026-03-31T00:00:00.000Z".to_owned()))
        );
    }

    #[test]
    fn entry_accepts_numeric_updated_and_deleted_at() {
        let input = json!({
            "id": "entry-4",
            "updated_at": 1775044526571_i64,
            "deleted_at": 1774990365021_i64
        });
        let entry: Entry = serde_json::from_value(input).expect("entry should deserialize");
        assert_eq!(entry.updated_at.as_deref(), Some("1775044526571"));
        assert_eq!(entry.deleted_at.as_deref(), Some("1774990365021"));
    }

    #[test]
    fn entry_rejects_non_scalar_updated_and_deleted_at_values() {
        let input = json!({
            "id": "entry-5",
            "updated_at": {"bad": true},
            "deleted_at": [1, 2, 3]
        });
        let err = serde_json::from_value::<Entry>(input).expect_err("entry should fail");
        assert!(
            err.to_string()
                .contains("expected string, number, or null, got"),
            "unexpected error: {err}"
        );
    }
}
