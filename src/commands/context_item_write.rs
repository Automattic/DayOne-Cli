use std::fs;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::store::sqlite::Store;

#[derive(Debug, Clone)]
pub struct ContextItemPutArgs {
    pub json: Option<String>,
    pub json_file: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContextItemDeleteArgs {
    pub id: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ContextItemWriteOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub operation: String,
    pub queued: bool,
    pub synced: bool,
    pub outbox_id: String,
    pub item_id: Option<String>,
    pub item_updated_at: Option<String>,
    pub item_deleted_at: Option<String>,
    pub item: Value,
}

pub async fn put(
    store: &Store,
    base_url: &str,
    args: ContextItemPutArgs,
) -> Result<ContextItemWriteOutput> {
    let payload = load_payload(args.json.as_deref(), args.json_file.as_deref())?;
    validate_put_payload(&payload)?;
    execute_write(store, base_url, "put", &payload).await
}

pub async fn delete(
    store: &Store,
    base_url: &str,
    args: ContextItemDeleteArgs,
) -> Result<ContextItemWriteOutput> {
    let (id, updated_at, deleted_at) = normalize_delete_inputs(args)?;

    let existing_item = store
        .get_json_row_by_id("context_library_items", &id)?
        .ok_or_else(|| anyhow!("context item '{id}' not found locally; run `dayone sync` first"))?;
    let existing_value: Value =
        serde_json::from_str(&existing_item).context("failed to parse local context item JSON")?;

    let mut payload = Map::new();
    payload.insert("id".to_owned(), Value::String(id));
    payload.insert(
        "content".to_owned(),
        required_string_from_existing(&existing_value, "content")?,
    );
    payload.insert(
        "category".to_owned(),
        required_string_from_existing(&existing_value, "category")?,
    );
    copy_if_present(&existing_value, &mut payload, "tags");
    copy_if_present(&existing_value, &mut payload, "source");
    copy_if_present(&existing_value, &mut payload, "embedding");
    copy_if_present(&existing_value, &mut payload, "needs_embedding");
    payload.insert("updated_at".to_owned(), Value::String(updated_at));
    payload.insert("deleted_at".to_owned(), Value::String(deleted_at));
    execute_write(store, base_url, "delete", &payload).await
}

async fn execute_write(
    store: &Store,
    base_url: &str,
    operation: &str,
    payload: &Map<String, Value>,
) -> Result<ContextItemWriteOutput> {
    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;
    let payload_value = Value::Object(payload.clone());
    let item_id = payload
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("payload field 'id' is required"))?;
    let updated_at = payload.get("updated_at").and_then(Value::as_str);
    let deleted_at = payload.get("deleted_at").and_then(Value::as_str);
    store.upsert_json_row_with_sync_state(
        "context_library_items",
        item_id,
        updated_at,
        deleted_at,
        Some(true),
        &payload_value.to_string(),
    )?;
    let outbox_id = format!("context_item:{item_id}");
    store.enqueue_outbox_item(
        &outbox_id,
        "context_item",
        "update",
        item_id,
        &payload_value.to_string(),
        now_epoch_ms(),
    )?;

    Ok(ContextItemWriteOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        operation: operation.to_owned(),
        queued: true,
        synced: false,
        outbox_id,
        item_id: extract_string_field(&payload_value, "id"),
        item_updated_at: extract_string_field(&payload_value, "updated_at"),
        item_deleted_at: extract_string_field(&payload_value, "deleted_at"),
        item: payload_value,
    })
}

fn normalize_delete_inputs(args: ContextItemDeleteArgs) -> Result<(String, String, String)> {
    let id = args.id.trim().to_owned();
    if id.is_empty() {
        bail!("id must not be empty");
    }

    let updated_at = args.updated_at.trim().to_owned();
    if updated_at.is_empty() {
        bail!("updated_at must not be empty");
    }

    let deleted_at = args
        .deleted_at
        .unwrap_or_else(|| updated_at.clone())
        .trim()
        .to_owned();
    if deleted_at.is_empty() {
        bail!("deleted_at must not be empty");
    }

    Ok((id, updated_at, deleted_at))
}

fn load_payload(json_inline: Option<&str>, json_file: Option<&str>) -> Result<Map<String, Value>> {
    if json_inline.is_some() && json_file.is_some() {
        bail!("use either --json or --json-file, not both");
    }
    let raw = if let Some(inline) = json_inline {
        inline.to_owned()
    } else if let Some(path) = json_file {
        fs::read_to_string(path).with_context(|| format!("failed to read JSON file '{path}'"))?
    } else {
        bail!("missing payload; provide --json or --json-file");
    };
    let value: Value = serde_json::from_str(&raw).context("invalid JSON payload")?;
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("context item payload must be a JSON object"))?;
    Ok(obj.clone())
}

fn validate_put_payload(payload: &Map<String, Value>) -> Result<()> {
    require_non_empty_string(payload, "id")?;
    require_non_empty_string(payload, "content")?;
    require_non_empty_string(payload, "category")?;
    require_non_empty_string(payload, "updated_at")?;
    Ok(())
}

fn require_non_empty_string(payload: &Map<String, Value>, key: &str) -> Result<()> {
    let value = payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("payload field '{key}' is required and must be a string"))?;
    if value.trim().is_empty() {
        bail!("payload field '{key}' must not be empty");
    }
    Ok(())
}

fn required_string_from_existing(existing: &Value, key: &str) -> Result<Value> {
    let value = existing
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("local context item is missing required field '{key}'"))?;
    Ok(Value::String(value.to_owned()))
}

fn copy_if_present(existing: &Value, out: &mut Map<String, Value>, key: &str) {
    if let Some(value) = existing.get(key) {
        out.insert(key.to_owned(), value.clone());
    }
}

fn extract_string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
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

    #[test]
    fn validates_required_put_fields() {
        let payload = serde_json::json!({
            "id": "ctx_1",
            "content": "hello",
            "category": "manual",
            "updated_at": "2026-03-11T00:00:00.000Z"
        });
        let obj = payload
            .as_object()
            .expect("payload should be object")
            .clone();
        assert!(validate_put_payload(&obj).is_ok());
    }

    #[test]
    fn rejects_missing_required_field() {
        let payload = serde_json::json!({
            "id": "ctx_1",
            "content": "hello",
            "updated_at": "2026-03-11T00:00:00.000Z"
        });
        let obj = payload
            .as_object()
            .expect("payload should be object")
            .clone();
        let err = validate_put_payload(&obj).expect_err("validation should fail");
        assert!(err.to_string().contains("category"));
    }

    #[test]
    fn delete_payload_requires_non_empty_id_and_updated_at() {
        let empty_id = ContextItemDeleteArgs {
            id: "  ".to_owned(),
            updated_at: "2026-03-11T00:00:00.000Z".to_owned(),
            deleted_at: None,
        };
        assert!(normalize_delete_inputs(empty_id).is_err());

        let empty_updated_at = ContextItemDeleteArgs {
            id: "ctx_1".to_owned(),
            updated_at: " ".to_owned(),
            deleted_at: None,
        };
        assert!(normalize_delete_inputs(empty_updated_at).is_err());
    }

    #[test]
    fn extracts_string_fields_from_value() {
        let v = serde_json::json!({"id":"ctx1"});
        assert_eq!(extract_string_field(&v, "id"), Some("ctx1".to_owned()));
    }
}
