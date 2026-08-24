use anyhow::Result;
use serde_json::json;

use crate::entry::attachments::StagedAttachment;
use crate::store::sqlite::{EntryAttachmentUpsert, Store};

pub fn upsert_entry_attachments(
    store: &Store,
    journal_id: &str,
    entry_id: &str,
    staged_attachments: &mut [StagedAttachment],
) -> Result<()> {
    for attachment in staged_attachments {
        let (
            thumbnail_content_type,
            thumbnail_md5,
            thumbnail_width,
            thumbnail_height,
            thumbnail_file_size_bytes,
            thumbnail_bytes,
        ) = match attachment.thumbnail.as_mut() {
            Some(thumbnail) => (
                Some(thumbnail.content_type.clone()),
                Some(thumbnail.md5.clone()),
                Some(thumbnail.width),
                Some(thumbnail.height),
                Some(thumbnail.file_size_bytes),
                Some(std::mem::take(&mut thumbnail.bytes)),
            ),
            None => (None, None, None, None, None, None),
        };
        store.upsert_entry_attachment(&EntryAttachmentUpsert {
            journal_id: journal_id.to_owned(),
            entry_id: entry_id.to_owned(),
            moment_id: attachment.moment_id.clone(),
            file_path: attachment.file_path.clone(),
            moment_type: attachment.moment_type.api_type_str().to_owned(),
            content_type: attachment.content_type.clone(),
            file_size_bytes: attachment.file_size_bytes,
            md5_body: attachment.md5_body.clone(),
            width: attachment.width,
            height: attachment.height,
            duration_seconds: attachment.duration_seconds,
            thumbnail_content_type,
            thumbnail_md5,
            thumbnail_width,
            thumbnail_height,
            thumbnail_file_size_bytes,
            thumbnail_bytes,
        })?;
    }
    Ok(())
}

pub fn enqueue_entry_and_media_outbox(
    store: &Store,
    journal_id: &str,
    entry_id: &str,
    entry_content: &serde_json::Value,
    queued_at_iso: &str,
    now_ms: i64,
    staged_attachments: &[StagedAttachment],
) -> Result<String> {
    let outbox_id = format!("entry:{journal_id}:{entry_id}");
    let outbox_payload = json!({
        "journal_id": journal_id,
        "entry_id": entry_id,
        "entry_json": entry_content,
        "queued_at": queued_at_iso,
        "queued_at_epoch_ms": now_ms
    });
    store.enqueue_outbox_item(
        &outbox_id,
        "entry",
        "update",
        &format!("{journal_id}:{entry_id}"),
        &outbox_payload.to_string(),
        now_ms,
    )?;
    for attachment in staged_attachments {
        let original_media_outbox_id = format!(
            "original_media:{journal_id}:{entry_id}:{}",
            &attachment.moment_id
        );
        let original_media_payload = json!({
            "journal_id": journal_id,
            "entry_id": entry_id,
            "moment_id": &attachment.moment_id,
            "queued_at": queued_at_iso,
            "queued_at_epoch_ms": now_ms
        });
        store.enqueue_outbox_item(
            &original_media_outbox_id,
            "original_media",
            "create",
            &format!("{journal_id}:{entry_id}:{}", &attachment.moment_id),
            &original_media_payload.to_string(),
            now_ms + 1,
        )?;
    }
    Ok(outbox_id)
}
