use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;

pub use crate::entry::attachments::MediaType;
use crate::entry::attachments::{
    MAX_ATTACHMENTS_PER_ENTRY, build_new_moments, normalize_or_generate_entry_id, now_epoch_ms,
    stage_attachments,
};
use crate::entry::body::{
    RequestedEntryDate, build_entry_content, local_midnight_epoch_ms_for_epoch_ms,
    parse_requested_entry_date, read_body,
};
use crate::entry::persistence::{enqueue_entry_and_media_outbox, upsert_entry_attachments};
use crate::store::sqlite::Store;
use crate::util::now_rfc3339_utc;

#[derive(Debug, Clone)]
pub struct EntryWriteArgs {
    pub journal_id: String,
    pub body: Option<String>,
    pub body_stdin: bool,
    pub entry_id: Option<String>,
    pub date: Option<String>,
    pub all_day: bool,
    pub attachments: Vec<PathBuf>,
    pub attachment_types: Vec<MediaType>,
}

#[derive(Debug, Serialize)]
pub struct EntryWriteOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub journal_id: String,
    pub entry_id: String,
    pub queued: bool,
    pub outbox_id: String,
    pub synced: bool,
}

pub async fn execute(
    store: &Store,
    base_url: &str,
    args: EntryWriteArgs,
) -> Result<EntryWriteOutput> {
    if !args.attachment_types.is_empty() && args.attachment_types.len() != args.attachments.len() {
        bail!(
            "attachment type count ({}) must match --attach count ({})",
            args.attachment_types.len(),
            args.attachments.len()
        );
    }
    if args.attachments.len() > MAX_ATTACHMENTS_PER_ENTRY {
        bail!(
            "too many attachments ({}); max supported by CLI is {}",
            args.attachments.len(),
            MAX_ATTACHMENTS_PER_ENTRY
        );
    }

    let body = read_body(args.body, args.body_stdin)?;
    if body.trim().is_empty() {
        bail!("entry body cannot be empty");
    }

    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;
    let _journal_row = store
        .get_json_row_by_id("journals", &args.journal_id)?
        .ok_or_else(|| {
            anyhow!(
                "journal '{}' not found locally; run `dayone sync` first",
                args.journal_id
            )
        })?;

    let entry_id = normalize_or_generate_entry_id(args.entry_id.as_deref())?;
    let mut staged_attachments = stage_attachments(&args.attachments, &args.attachment_types)?;
    let new_moments = build_new_moments(&staged_attachments);
    let now_ms = now_epoch_ms();
    let mut requested_entry_date = parse_requested_entry_date(args.date.as_deref(), args.all_day)?;
    if args.all_day && requested_entry_date.is_none() {
        requested_entry_date = Some(RequestedEntryDate {
            epoch_ms: local_midnight_epoch_ms_for_epoch_ms(now_ms)?,
            is_all_day: true,
        });
    }
    let now_iso = now_rfc3339_utc();
    let existing_entry = store
        .get_entry_json_row_with_extra_fields(&entry_id)?
        .and_then(|(row, extra_fields_json)| {
            serde_json::from_str::<Value>(&row)
                .ok()
                .map(|value| (value, extra_fields_json))
        });
    let mut entry_content = build_entry_content(
        existing_entry.as_ref().map(|(value, _)| value),
        existing_entry
            .as_ref()
            .and_then(|(_, extra_fields_json)| extra_fields_json.as_deref()),
        &entry_id,
        body,
        requested_entry_date,
        now_ms,
        &new_moments,
    )?;
    // Writing over a locally deleted entry is an undelete: any tombstone left
    // in the rebuilt content would later make sync flip the queued update back
    // into a delete.
    if let Some(obj) = entry_content.as_object_mut() {
        obj.remove("deleted_at");
        obj.remove("deletedAt");
    }
    store.upsert_entry_json_row_with_edit_date(
        &entry_id,
        &args.journal_id,
        Some(&now_iso),
        Some(&now_iso),
        None,
        &entry_content.to_string(),
    )?;
    upsert_entry_attachments(store, &args.journal_id, &entry_id, &mut staged_attachments)?;
    let outbox_id = enqueue_entry_and_media_outbox(
        store,
        &args.journal_id,
        &entry_id,
        &entry_content,
        &now_iso,
        now_ms,
        &staged_attachments,
    )?;

    Ok(EntryWriteOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        journal_id: args.journal_id,
        entry_id,
        queued: true,
        outbox_id,
        synced: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::entry_delete::{self, EntryDeleteArgs};
    use crate::store::sqlite::Store;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn setup_store(base_url: &str) -> Store {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("dayone-cli-entry-write-{unique}.db"));
        let store = Store::open_at(&path).expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url(base_url)
            .expect("profile should create");
        store
            .save_auth_session(
                profile.id,
                "tok",
                "2026-03-17T00:00:00.000Z",
                r#"{"id":"test-user"}"#,
            )
            .expect("session should save");
        store
            .upsert_json_row(
                "journals",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"journal-1","name":"Journal 1"}"#,
            )
            .expect("journal should save");
        store
    }

    fn write_args(entry_id: &str, body: &str) -> EntryWriteArgs {
        EntryWriteArgs {
            journal_id: "journal-1".to_owned(),
            body: Some(body.to_owned()),
            body_stdin: false,
            entry_id: Some(entry_id.to_owned()),
            date: None,
            all_day: false,
            attachments: Vec::new(),
            attachment_types: Vec::new(),
        }
    }

    /// Writing over a locally deleted entry is an undelete: no `deleted_at`
    /// remnant may survive in the stored blob or the queued update, otherwise
    /// reconciliation later flips the queued update back into a delete.
    #[tokio::test]
    async fn write_after_delete_clears_tombstone_everywhere_and_queues_update() {
        let base_url = "https://stg.dayone.me";
        let store = setup_store(base_url);
        let entry_id = "A65AFA22B98A0DEC62EAAD3CD1CCF45D";
        store
            .upsert_entry_json_row_with_edit_date(
                entry_id,
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                Some("2026-03-17T00:00:00.000Z"),
                None,
                &format!(r#"{{"id":"{entry_id}","body":"original"}}"#),
            )
            .expect("entry should save");
        entry_delete::execute(
            &store,
            base_url,
            EntryDeleteArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: entry_id.to_owned(),
            },
        )
        .expect("delete should succeed");

        execute(&store, base_url, write_args(entry_id, "rewritten body"))
            .await
            .expect("write should succeed");

        let row = store
            .get_json_row_by_id("entries", entry_id)
            .expect("query should work")
            .expect("entry should exist");
        let value: Value = serde_json::from_str(&row).expect("entry row should parse");
        assert!(
            value.get("deleted_at").is_none() && value.get("deletedAt").is_none(),
            "rewrite must clear the tombstone from the stored entry blob, got: {row}"
        );

        let conn = store.connect().expect("connection should open");
        let (payload_json, operation): (String, String) = conn
            .query_row(
                "SELECT payload_json, operation FROM sync_outbox WHERE id = ?1",
                [format!("entry:journal-1:{entry_id}")],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("entry outbox item should exist");
        assert_eq!(operation, "update", "rewrite must requeue as an update");
        let payload: Value =
            serde_json::from_str(&payload_json).expect("outbox payload should parse");
        let entry_json = payload
            .get("entry_json")
            .expect("update payload should carry an entry snapshot");
        assert!(
            entry_json.get("deleted_at").is_none() && entry_json.get("deletedAt").is_none(),
            "queued update snapshot must not carry a tombstone, got: {entry_json}"
        );
    }
}
