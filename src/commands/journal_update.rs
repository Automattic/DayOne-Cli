//! `dayone journal update` — modify an existing journal's metadata.
//!
//! The change is applied to the local SQLite copy and an outbox item is queued
//! so the next `dayone sync` pushes it to the server (`PUT /v3/sync/journals/{id}`).
//! This command performs no network I/O itself.

use std::fs;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::commands::journal_create::EDITABLE_JOURNAL_FIELDS;
use crate::store::sqlite::Store;
use crate::util::now_rfc3339_utc;

#[derive(Debug, Clone)]
pub struct JournalUpdateArgs {
    pub journal_id: String,
    pub json: Option<String>,
    pub json_file: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub sort_method: Option<String>,
    pub hide_on_this_day: Option<bool>,
    pub hide_all_entries: Option<bool>,
    pub conceal: Option<bool>,
    pub add_location_to_new_entries: Option<bool>,
    pub comments_disabled: Option<bool>,
    pub template_id: Option<String>,
    pub preset_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JournalUpdateOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub journal_id: String,
    pub queued: bool,
    pub outbox_id: String,
    pub journal: Value,
}

pub async fn execute(
    store: &Store,
    base_url: &str,
    args: JournalUpdateArgs,
) -> Result<JournalUpdateOutput> {
    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;

    let journal_id = args.journal_id.trim().to_owned();
    if journal_id.is_empty() {
        bail!("journal id is required");
    }

    let existing = store
        .get_json_row_by_id("journals", &journal_id)?
        .ok_or_else(|| {
            anyhow!(
                "journal '{journal_id}' not found locally; run `dayone sync` first or check the id"
            )
        })?;
    let mut journal: Map<String, Value> = serde_json::from_str::<Value>(&existing)
        .context("invalid stored journal JSON")?
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("stored journal '{journal_id}' is not a JSON object"))?;

    let updates = collect_updates(&args)?;
    if updates.is_empty() {
        bail!(
            "no fields to update; pass at least one of --name/--description/--color/etc. or --json"
        );
    }
    for (key, value) in updates {
        journal.insert(key, value);
    }

    // Keep the id canonical and stamp the edit time for local ordering.
    journal.insert("id".to_owned(), Value::String(journal_id.clone()));
    let now_iso = now_rfc3339_utc();
    journal.insert("updated_at".to_owned(), Value::String(now_iso));

    let local_journal = Value::Object(journal);
    let serialized = local_journal.to_string();
    // The outbox handler re-reads the local journal by id at push time. The
    // queue stores only a row hash so an edit made during a push is requeued.
    // Save both together so pull cannot enter between the local writes.
    store.upsert_journal_and_enqueue_update(
        &journal_id,
        local_journal.get("updated_at").and_then(Value::as_str),
        local_journal.get("deleted_at").and_then(Value::as_str),
        &serialized,
    )?;
    let outbox_id = format!("journal:update:{journal_id}");

    Ok(JournalUpdateOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        journal_id,
        queued: true,
        outbox_id,
        journal: local_journal,
    })
}

/// Builds the set of field overrides from `--json`/`--json-file` (applied first)
/// and the convenience flags (which take precedence when both are present).
fn collect_updates(args: &JournalUpdateArgs) -> Result<Map<String, Value>> {
    let mut updates = load_json_overrides(args)?;

    // Convenience flags override any value supplied in --json. `--description`
    // maps to the server's `description_v2` field, matching `journal create`.
    let flag_fields: [(&str, Option<Value>); 11] = [
        ("name", args.name.clone().map(Value::String)),
        (
            "description_v2",
            args.description.clone().map(Value::String),
        ),
        ("color", args.color.clone().map(Value::String)),
        ("sort_method", args.sort_method.clone().map(Value::String)),
        ("hide_on_this_day", args.hide_on_this_day.map(Value::Bool)),
        ("hide_all_entries", args.hide_all_entries.map(Value::Bool)),
        ("conceal", args.conceal.map(Value::Bool)),
        (
            "add_location_to_new_entries",
            args.add_location_to_new_entries.map(Value::Bool),
        ),
        ("comments_disabled", args.comments_disabled.map(Value::Bool)),
        ("template_id", args.template_id.clone().map(Value::String)),
        ("preset_id", args.preset_id.clone().map(Value::String)),
    ];
    for (key, value) in flag_fields {
        if let Some(value) = value {
            updates.insert(key.to_owned(), value);
        }
    }

    // A name, if provided, must be a non-empty string.
    if let Some(Value::String(name)) = updates.get("name")
        && name.trim().is_empty()
    {
        bail!("journal name cannot be empty");
    }

    Ok(updates)
}

fn load_json_overrides(args: &JournalUpdateArgs) -> Result<Map<String, Value>> {
    if args.json.is_some() && args.json_file.is_some() {
        bail!("use either --json or --json-file, not both");
    }
    let raw = if let Some(inline) = args.json.as_deref() {
        inline.to_owned()
    } else if let Some(path) = args.json_file.as_deref() {
        fs::read_to_string(path).with_context(|| format!("failed to read JSON file '{path}'"))?
    } else {
        return Ok(Map::new());
    };
    let value: Value = serde_json::from_str(&raw).context("invalid JSON payload")?;
    let mut object = value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("journal update JSON payload must be an object"))?;
    // Keep only editable metadata fields; silently drop everything else (the
    // managed `encryption` vault, server identity/state, etc.) so a full journal
    // JSON can be passed without corrupting managed data.
    object.retain(|key, _| EDITABLE_JOURNAL_FIELDS.contains(&key.as_str()));
    Ok(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_with(journal_id: &str) -> JournalUpdateArgs {
        JournalUpdateArgs {
            journal_id: journal_id.to_owned(),
            json: None,
            json_file: None,
            name: None,
            description: None,
            color: None,
            sort_method: None,
            hide_on_this_day: None,
            hide_all_entries: None,
            conceal: None,
            add_location_to_new_entries: None,
            comments_disabled: None,
            template_id: None,
            preset_id: None,
        }
    }

    #[test]
    fn collect_updates_maps_flags() {
        let mut args = args_with("j1");
        args.name = Some("Renamed".to_owned());
        args.description = Some("New desc".to_owned());
        args.color = Some("#FF0000".to_owned());
        args.hide_all_entries = Some(true);

        let updates = collect_updates(&args).expect("updates");
        assert_eq!(updates.get("name").and_then(Value::as_str), Some("Renamed"));
        assert_eq!(
            updates.get("description_v2").and_then(Value::as_str),
            Some("New desc")
        );
        assert_eq!(
            updates.get("color").and_then(Value::as_str),
            Some("#FF0000")
        );
        assert_eq!(
            updates.get("hide_all_entries").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn collect_updates_rejects_empty_name() {
        let mut args = args_with("j1");
        args.name = Some("   ".to_owned());
        assert!(collect_updates(&args).is_err());
    }

    #[test]
    fn collect_updates_flag_overrides_json() {
        let mut args = args_with("j1");
        args.json = Some(r##"{"color":"#000000","name":"FromJson"}"##.to_owned());
        args.name = Some("FromFlag".to_owned());

        let updates = collect_updates(&args).expect("updates");
        // Flag wins over the JSON-supplied value.
        assert_eq!(
            updates.get("name").and_then(Value::as_str),
            Some("FromFlag")
        );
        // JSON-only fields are preserved.
        assert_eq!(
            updates.get("color").and_then(Value::as_str),
            Some("#000000")
        );
    }

    #[test]
    fn collect_updates_empty_when_nothing_provided() {
        let updates = collect_updates(&args_with("j1")).expect("updates");
        assert!(updates.is_empty());
    }

    #[test]
    fn json_and_json_file_are_mutually_exclusive() {
        let mut args = args_with("j1");
        args.json = Some("{}".to_owned());
        args.json_file = Some("/tmp/x.json".to_owned());
        assert!(collect_updates(&args).is_err());
    }

    #[test]
    fn json_keeps_only_editable_fields() {
        let mut args = args_with("j1");
        // Editable fields kept; encryption vault + managed identity/state dropped.
        args.json = Some(
            r#"{"name":"x","conceal":true,"encryption":{"vault":{}},
                "participants":[{"id":"1"}],"owner_id":"9","state":"active"}"#
                .to_owned(),
        );
        let updates = collect_updates(&args).expect("non-editable fields are dropped, not errors");
        assert_eq!(updates.get("name").and_then(Value::as_str), Some("x"));
        assert_eq!(updates.get("conceal").and_then(Value::as_bool), Some(true));
        for dropped in ["encryption", "participants", "owner_id", "state"] {
            assert!(!updates.contains_key(dropped), "{dropped} must be dropped");
        }
    }
}
