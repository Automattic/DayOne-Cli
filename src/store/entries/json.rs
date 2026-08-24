use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::store::sqlite::Store;
use crate::store::{StoreError, StoreResult};

pub(crate) const KNOWN_ENTRY_FIELD_KEYS: &[&str] = &[
    "date",
    "title",
    "body",
    "richTextJSON",
    "timeZone",
    "tags",
    "moments",
    "clientMeta",
];

pub(crate) const RESERVED_ENTRY_FIELD_KEYS: &[&str] = &[
    "id",
    "journal_id",
    "updated_at",
    "user_edit_date",
    "deleted_at",
    "is_deleted",
    "needs_sync",
    "revision",
    "payload",
];

pub(crate) const DROPPED_ENTRY_FIELD_KEYS: &[&str] = &[
    // Historical CLI-only fallback for decrypted D1 payloads that were not valid JSON.
    // Treat as intentionally discarded local scar tissue, not as API content and not as
    // an unknown field to preserve.
    "decrypted_text",
];

#[inline]
fn should_preserve_extra_field(key: &str) -> bool {
    !KNOWN_ENTRY_FIELD_KEYS.contains(&key)
        && !RESERVED_ENTRY_FIELD_KEYS.contains(&key)
        && !DROPPED_ENTRY_FIELD_KEYS.contains(&key)
}

/// Unknown fields from the root object and from `payload` (when present), merged so root wins
/// on duplicate keys — matching [`entry_field_value`] precedence for known columns.
pub(crate) fn collect_merged_extra_fields(entry_value: &Value) -> Map<String, Value> {
    let Some(root) = entry_value.as_object() else {
        return Map::new();
    };
    let payload = entry_value.get("payload").and_then(Value::as_object);

    let mut extra_fields = Map::new();

    if let Some(p) = payload {
        for (k, v) in p {
            if should_preserve_extra_field(k) {
                extra_fields.insert(k.clone(), v.clone());
            }
        }
    }

    for (k, v) in root {
        if should_preserve_extra_field(k) {
            extra_fields.insert(k.clone(), v.clone());
        }
    }

    extra_fields
}

pub(crate) fn parse_iso_to_epoch_ms(iso_str: &str) -> StoreResult<f64> {
    let dt = OffsetDateTime::parse(iso_str, &Rfc3339).map_err(|err| {
        StoreError::invalid_input(format!("failed to parse ISO date {iso_str}: {err}"))
    })?;
    let epoch_ms = (dt.unix_timestamp_nanos() / 1_000_000) as f64;
    Ok(epoch_ms)
}

fn parse_extra_fields_json(raw: Option<&str>) -> Map<String, Value> {
    let Some(trimmed_str) = raw.map(str::trim).filter(|trimmed| !trimmed.is_empty()) else {
        return Map::new();
    };
    match serde_json::from_str::<Value>(trimmed_str) {
        Ok(Value::Object(obj)) => obj,
        _ => Map::new(),
    }
}

fn entry_field_value(value: &Value, key: &str) -> Option<Value> {
    value.get(key).cloned().or_else(|| {
        value
            .get("payload")
            .and_then(|payload| payload.get(key))
            .cloned()
    })
}

fn extract_extra_fields_json(value: &Value) -> Option<String> {
    let extra_fields = collect_merged_extra_fields(value);
    if extra_fields.is_empty() {
        None
    } else {
        Some(Value::Object(extra_fields).to_string())
    }
}

fn rich_text_json_has_empty_contents(value: &Value) -> bool {
    value
        .get("contents")
        .or_else(|| value.get("nodes"))
        .and_then(Value::as_array)
        .map(|contents| contents.is_empty())
        .unwrap_or(false)
}

fn normalize_rich_text_json(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        Value::String(s) => {
            if serde_json::from_str::<Value>(s.trim())
                .ok()
                .map(|parsed| rich_text_json_has_empty_contents(&parsed))
                .unwrap_or(false)
            {
                None
            } else {
                Some(s.to_owned())
            }
        }
        Value::Object(obj) if obj.is_empty() => None,
        Value::Object(_) if rich_text_json_has_empty_contents(value) => None,
        Value::Array(items) if items.is_empty() => None,
        Value::Object(_) | Value::Array(_) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn rebuild_entry_content(
    local: &Value,
    extra_fields_json: Option<&str>,
    entry_id: &str,
) -> Value {
    let local_deleted_at = local.get("deleted_at").cloned();
    let mut content = parse_extra_fields_json(extra_fields_json);
    // Defensively strip known/reserved keys that may have leaked into
    // extra_fields_json from older migrations or stale data.
    content.retain(|k, _| should_preserve_extra_field(k));
    if content.is_empty() {
        content = collect_merged_extra_fields(local);
    }

    for key in KNOWN_ENTRY_FIELD_KEYS {
        if let Some(value) = entry_field_value(local, key) {
            content.insert((*key).to_owned(), value);
        }
    }

    content.insert("id".to_owned(), Value::String(entry_id.to_owned()));
    if let Some(deleted_at) = local_deleted_at {
        content.insert("deleted_at".to_owned(), deleted_at);
    }

    Value::Object(content)
}

fn parse_entry_data_json(data_json: &str) -> StoreResult<Value> {
    serde_json::from_str(data_json)
        .map_err(|_| StoreError::invalid_input("invalid JSON in entry data_json"))
}

fn strip_dropped_entry_fields(value: &mut Value) -> bool {
    let Some(root) = value.as_object_mut() else {
        return false;
    };

    let mut changed = false;
    for key in DROPPED_ENTRY_FIELD_KEYS {
        changed |= root.remove(*key).is_some();
    }

    if let Some(payload) = root.get_mut("payload").and_then(Value::as_object_mut) {
        for key in DROPPED_ENTRY_FIELD_KEYS {
            changed |= payload.remove(*key).is_some();
        }
    }

    changed
}

fn prepare_entry_data_json(data_json: &str) -> StoreResult<(String, ExtractedEntryFields)> {
    let mut value = parse_entry_data_json(data_json)?;
    let changed = strip_dropped_entry_fields(&mut value);
    let fields = extract_entry_fields(&value)?;
    let prepared_data_json = if changed {
        value.to_string()
    } else {
        data_json.to_owned()
    };
    Ok((prepared_data_json, fields))
}

fn extract_entry_fields(value: &Value) -> StoreResult<ExtractedEntryFields> {
    if !value.is_object() {
        return Err(StoreError::invalid_input(
            "entry data_json must be a JSON object",
        ));
    }

    // Helper: get a non-null field from root, falling back to payload.
    // Filters `Value::Null` before the fallback so that `{"date": null, "payload": {"date": 123}}`
    // correctly falls through to the payload value — matching the SQL migration semantics.
    let get_field = |key: &str| -> Option<&Value> {
        value
            .get(key)
            .filter(|v| !v.is_null())
            .or_else(|| value.get("payload").and_then(|p| p.get(key)))
    };

    let date = get_field("date").and_then(|v| {
        if let Some(num) = v.as_f64() {
            Some(num)
        } else if let Some(s) = v.as_str() {
            // Try numeric string first (epoch ms), then ISO 8601
            s.parse::<f64>()
                .ok()
                .or_else(|| parse_iso_to_epoch_ms(s).ok())
        } else {
            None
        }
    });

    let title = get_field("title").and_then(Value::as_str).map(String::from);

    let body = get_field("body").and_then(Value::as_str).map(String::from);

    let rich_text_json = get_field("richTextJSON").and_then(normalize_rich_text_json);

    let time_zone = get_field("timeZone")
        .and_then(|v| if v.is_null() { None } else { v.as_str() })
        .map(String::from);

    let tags_json = get_field("tags").and_then(|v| {
        if v.is_null() {
            None
        } else {
            Some(v.to_string())
        }
    });

    let moments_json = get_field("moments").and_then(|v| {
        if v.is_null() {
            None
        } else {
            Some(v.to_string())
        }
    });

    let client_meta_json = get_field("clientMeta").and_then(|v| {
        if v.is_null() {
            None
        } else {
            Some(v.to_string())
        }
    });

    let user_edit_date = value
        .get("revision")
        .and_then(|revision| revision.get("editDate"))
        .filter(|v| !v.is_null())
        .and_then(user_edit_date_value_to_storage_string);

    Ok(ExtractedEntryFields {
        date,
        user_edit_date,
        title,
        body,
        rich_text_json,
        time_zone,
        tags_json,
        moments_json,
        client_meta_json,
        extra_fields_json: extract_extra_fields_json(value),
    })
}

fn user_edit_date_value_to_storage_string(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        _ => None,
    }
}

#[derive(Default)]
struct ExtractedEntryFields {
    date: Option<f64>,
    user_edit_date: Option<String>,
    title: Option<String>,
    body: Option<String>,
    rich_text_json: Option<String>,
    time_zone: Option<String>,
    tags_json: Option<String>,
    moments_json: Option<String>,
    client_meta_json: Option<String>,
    extra_fields_json: Option<String>,
}

/// A single entry row with the metadata columns needed to serve a full
/// single-entry read (`dayone entry read`) alongside the stored JSON.
#[derive(Debug)]
pub struct EntryRowRecord {
    pub journal_id: String,
    pub updated_at: Option<String>,
    pub user_edit_date: Option<String>,
    pub data_json: String,
    pub extra_fields_json: Option<String>,
}

impl Store {
    pub fn get_entry_row_record(&self, id: &str) -> StoreResult<Option<EntryRowRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT journal_id, updated_at, user_edit_date, data_json, extra_fields_json
             FROM entries WHERE id = ?1 LIMIT 1",
            params![id],
            |row| {
                Ok(EntryRowRecord {
                    journal_id: row.get(0)?,
                    updated_at: row.get(1)?,
                    user_edit_date: row.get(2)?,
                    data_json: row.get(3)?,
                    extra_fields_json: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_entry_json_row_with_extra_fields(
        &self,
        id: &str,
    ) -> StoreResult<Option<(String, Option<String>)>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT data_json, extra_fields_json FROM entries WHERE id = ?1 LIMIT 1",
            params![id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_entry_json_row(
        &self,
        id: &str,
        journal_id: &str,
        updated_at: Option<&str>,
        deleted_at: Option<&str>,
        data_json: &str,
    ) -> StoreResult<()> {
        let (data_json, fields) = prepare_entry_data_json(data_json)?;
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO entries (
              id, journal_id, date, user_edit_date, updated_at, deleted_at, is_deleted,
              title, body, rich_text_json, time_zone,
              tags_json, moments_json, client_meta_json, extra_fields_json, data_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(id) DO UPDATE SET
              journal_id = excluded.journal_id,
              date = excluded.date,
              user_edit_date = excluded.user_edit_date,
              updated_at = excluded.updated_at,
              deleted_at = excluded.deleted_at,
              is_deleted = excluded.is_deleted,
              title = excluded.title,
              body = excluded.body,
              rich_text_json = excluded.rich_text_json,
              time_zone = excluded.time_zone,
              tags_json = excluded.tags_json,
              moments_json = excluded.moments_json,
              client_meta_json = excluded.client_meta_json,
              extra_fields_json = excluded.extra_fields_json,
              data_json = excluded.data_json
            "#,
            params![
                id,
                journal_id,
                fields.date,
                fields.user_edit_date,
                updated_at,
                deleted_at,
                if deleted_at.is_some() { 1 } else { 0 },
                fields.title,
                fields.body,
                fields.rich_text_json,
                fields.time_zone,
                fields.tags_json,
                fields.moments_json,
                fields.client_meta_json,
                fields.extra_fields_json,
                data_json
            ],
        )?;
        Ok(())
    }

    pub fn upsert_entry_json_row_with_edit_date(
        &self,
        id: &str,
        journal_id: &str,
        user_edit_date: Option<&str>,
        updated_at: Option<&str>,
        deleted_at: Option<&str>,
        data_json: &str,
    ) -> StoreResult<()> {
        let (data_json, fields) = prepare_entry_data_json(data_json)?;
        let effective_user_edit_date = user_edit_date.or(fields.user_edit_date.as_deref());
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO entries (
              id, journal_id, date, user_edit_date, updated_at, deleted_at, is_deleted,
              title, body, rich_text_json, time_zone,
              tags_json, moments_json, client_meta_json, extra_fields_json, data_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(id) DO UPDATE SET
              journal_id = excluded.journal_id,
              date = excluded.date,
              user_edit_date = excluded.user_edit_date,
              updated_at = excluded.updated_at,
              deleted_at = excluded.deleted_at,
              is_deleted = excluded.is_deleted,
              title = excluded.title,
              body = excluded.body,
              rich_text_json = excluded.rich_text_json,
              time_zone = excluded.time_zone,
              tags_json = excluded.tags_json,
              moments_json = excluded.moments_json,
              client_meta_json = excluded.client_meta_json,
              extra_fields_json = excluded.extra_fields_json,
              data_json = excluded.data_json
            "#,
            params![
                id,
                journal_id,
                fields.date,
                effective_user_edit_date,
                updated_at,
                deleted_at,
                if deleted_at.is_some() { 1 } else { 0 },
                fields.title,
                fields.body,
                fields.rich_text_json,
                fields.time_zone,
                fields.tags_json,
                fields.moments_json,
                fields.client_meta_json,
                fields.extra_fields_json,
                data_json
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod rich_text_json_tests {
    use super::normalize_rich_text_json;
    use serde_json::{Value, json};

    #[test]
    fn normalize_rich_text_json_ignores_empty_shapes() {
        assert_eq!(normalize_rich_text_json(&Value::Null), None);
        assert_eq!(normalize_rich_text_json(&json!("")), None);
        assert_eq!(normalize_rich_text_json(&json!("   ")), None);
        assert_eq!(normalize_rich_text_json(&json!({})), None);
        assert_eq!(normalize_rich_text_json(&json!([])), None);
        assert_eq!(normalize_rich_text_json(&json!({"contents": []})), None);
        assert_eq!(normalize_rich_text_json(&json!({"nodes": []})), None);
        assert_eq!(normalize_rich_text_json(&json!("{\"contents\":[]}")), None);
    }

    #[test]
    fn normalize_rich_text_json_preserves_meaningful_values() {
        assert_eq!(
            normalize_rich_text_json(&json!({"contents": [{"text": "hello"}]})),
            Some(r#"{"contents":[{"text":"hello"}]}"#.to_owned())
        );
        assert_eq!(
            normalize_rich_text_json(&json!("{\"contents\":[{\"text\":\"hello\"}]}")),
            Some(r#"{"contents":[{"text":"hello"}]}"#.to_owned())
        );
    }
}

#[cfg(test)]
mod field_key_list_tests {
    use super::{DROPPED_ENTRY_FIELD_KEYS, KNOWN_ENTRY_FIELD_KEYS, RESERVED_ENTRY_FIELD_KEYS};

    #[test]
    fn known_entry_field_keys_cardinality() {
        assert_eq!(
            KNOWN_ENTRY_FIELD_KEYS.len(),
            8,
            "bump count when adding a known column field; extend round-trip coverage in store tests"
        );
    }

    #[test]
    fn reserved_entry_field_keys_cardinality() {
        assert_eq!(
            RESERVED_ENTRY_FIELD_KEYS.len(),
            9,
            "bump count when adding a reserved/metadata field; extend strip/round-trip tests if needed"
        );
    }

    #[test]
    fn dropped_entry_field_keys_cardinality() {
        assert_eq!(
            DROPPED_ENTRY_FIELD_KEYS.len(),
            1,
            "bump count when adding a dropped field; extend strip/round-trip tests if needed"
        );
    }
}
