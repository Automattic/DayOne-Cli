use super::*;
use md5::{Digest, Md5};

pub(crate) fn build_outbox_crypto_context(store: &Store, profile_id: i64) -> Result<CryptoContext> {
    let master_key = store.get_encryption_key_for_profile(profile_id)?;
    let user_identity_key = store.get_user_identity_key_json()?;
    let content_key_bundle = store.get_json_row_by_id("user_keys", "1")?;
    Ok(build_crypto_context_from_sources(
        master_key,
        user_identity_key.as_deref(),
        content_key_bundle.as_deref(),
    ))
}

pub(crate) async fn push_original_media_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    let payload = decode_original_media_outbox_payload(item)?;
    let journal_id = payload.journal_id.as_str();
    let entry_id = payload.entry_id.as_str();
    let moment_id = payload.moment_id.as_str();
    let attachment = store
        .get_entry_attachment(journal_id, entry_id, moment_id)?
        .ok_or_else(|| {
            OutboxProcessError::NonRetryable {
                reason: format!(
                "missing local attachment row for journal_id={journal_id} entry_id={entry_id} moment_id={moment_id}"
                ),
            }
        })?;
    if !attachment.entry_associated {
        return Err(OutboxProcessError::Retryable(anyhow!(
            "entry attachment not yet associated for journal_id={journal_id} entry_id={entry_id} moment_id={moment_id}"
        )));
    }

    // For E2E journals the original media bytes must be client-side encrypted
    // before upload so the server never sees plaintext. Server-side S3
    // encryption stays on as defense-in-depth. Plaintext journals continue to
    // stream the original file directly.
    //
    // This must fail closed: we always resolve the journal's encryption status
    // before uploading, so a missing/corrupt journal row errors out instead of
    // silently downgrading an E2E journal to a plaintext upload.
    let journal = load_local_journal_value(store, journal_id)?;
    let encrypted_media = if journal_is_e2e_encrypted(&journal) {
        let master_key = store.get_encryption_key_for_profile(profile_id)?;
        let user_keys_json = store.get_user_identity_key_json()?;
        let media_bytes = std::fs::read(&attachment.file_path).map_err(|err| {
            OutboxProcessError::Retryable(anyhow!(
                "failed to read media file '{}' for encryption: {err}",
                attachment.file_path
            ))
        })?;
        let ciphertext = encrypt_original_media_for_journal(
            &journal,
            &media_bytes,
            master_key.as_deref(),
            user_keys_json.as_deref(),
        )?;
        Some(ciphertext)
    } else {
        None
    };

    let upload_base = resolve_sync_upload_base_url(store)?;
    let upload_url = format!(
        "{}/v2-{}-{}-{}",
        upload_base.trim_end_matches('/'),
        journal_id,
        entry_id,
        moment_id
    );
    // The S3 object metadata md5 describes the bytes actually stored, i.e. the
    // ciphertext for E2E journals (the original-bytes md5 still lives inside the
    // encrypted entry content). Plaintext journals keep the original md5.
    let object_md5 = match encrypted_media.as_ref() {
        Some(ciphertext) => format!("{:x}", Md5::digest(ciphertext)),
        None => attachment.md5_body.clone(),
    };
    let headers = vec![
        ("x-amz-meta-entry-id", entry_id.to_owned()),
        ("x-amz-meta-journal-id", journal_id.to_owned()),
        ("x-amz-meta-moment-id", moment_id.to_owned()),
        ("x-amz-meta-md5", object_md5),
        ("x-amz-acl", "bucket-owner-full-control".to_owned()),
        ("x-amz-server-side-encryption", "AES256".to_owned()),
    ];
    let result = match encrypted_media.as_ref() {
        Some(ciphertext) => {
            api.put_bytes_to_absolute_url(
                &upload_url,
                "application/octet-stream",
                &headers,
                ciphertext,
            )
            .await
        }
        None => {
            api.put_file_to_absolute_url(
                &upload_url,
                &attachment.content_type,
                &headers,
                &attachment.file_path,
                attachment.file_size_bytes,
            )
            .await
        }
    };
    if let Err(err) = &result {
        let _ =
            store.mark_entry_attachment_error(journal_id, entry_id, moment_id, &err.to_string());
    }
    result?;
    store.mark_entry_attachment_original_uploaded(journal_id, entry_id, moment_id)?;
    Ok(())
}

/// Loads the local journal row, failing closed so callers never proceed without
/// a known encryption status. A DB error or missing row is retryable (the
/// journal may not have synced locally yet); an unparseable row is a
/// non-retryable data-corruption error.
fn load_local_journal_value(store: &Store, journal_id: &str) -> OutboxProcessResult<Value> {
    let row = store
        .get_json_row_by_id("journals", journal_id)?
        .ok_or_else(|| {
            OutboxProcessError::Retryable(anyhow!(
                "missing local journal row for journal_id={journal_id}; cannot determine encryption status before media upload"
            ))
        })?;
    serde_json::from_str::<Value>(&row).map_err(|err| OutboxProcessError::NonRetryable {
        reason: format!("local journal row for journal_id={journal_id} is not valid JSON: {err}"),
    })
}

pub(crate) fn resolve_sync_upload_base_url(store: &Store) -> Result<String> {
    let user_profile = store
        .get_json_row_by_id("user_profile", "1")?
        .ok_or_else(|| anyhow!("missing user profile locally; run `dayone sync` first"))?;
    let payload: Value = serde_json::from_str(&user_profile)?;
    let url = payload
        .get("sync_upload_base_url")
        .or_else(|| payload.get("syncUploadBaseUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!("user profile missing sync_upload_base_url; run `dayone sync` first")
        })?;
    Ok(url.to_owned())
}

pub(crate) fn build_entry_thumbnail_parts(
    attachments: &[EntryAttachmentRow],
) -> Vec<MultipartBinaryPart> {
    let mut parts = Vec::new();
    for attachment in attachments {
        let (Some(thumbnail_bytes), Some(thumbnail_content_type)) = (
            attachment.thumbnail_bytes.as_ref(),
            attachment.thumbnail_content_type.as_ref(),
        ) else {
            continue;
        };
        let part_name = format!("thumbnail.{}", attachment.moment_id);
        parts.push(MultipartBinaryPart {
            name: part_name.clone(),
            file_name: part_name,
            content_type: thumbnail_content_type.clone(),
            bytes: thumbnail_bytes.clone(),
        });
    }
    parts
}

pub(crate) fn build_entry_content_from_local(
    local: &Value,
    extra_fields_json: Option<&str>,
    entry_id: &str,
) -> Value {
    rebuild_entry_content(local, extra_fields_json, entry_id)
}

pub(crate) fn value_to_string(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_owned();
    }
    if let Some(i) = v.as_i64() {
        return i.to_string();
    }
    if let Some(u) = v.as_u64() {
        return u.to_string();
    }
    if let Some(f) = v.as_f64() {
        return f.to_string();
    }
    v.to_string()
}

pub(crate) fn build_entry_envelope_moments(moments: &[Moment]) -> Value {
    let mut out = Vec::with_capacity(moments.len());
    for moment in moments {
        let mut env = serde_json::Map::new();
        if let Some(id) = moment.id.as_ref() {
            env.insert("id".to_owned(), Value::String(id.as_ref().to_owned()));
        }
        if let Some(content_type) = moment.content_type.as_ref() {
            env.insert(
                "contentType".to_owned(),
                Value::String(content_type.clone()),
            );
        }
        if let Some(kind) = moment.kind.as_ref() {
            env.insert("momentType".to_owned(), Value::String(kind.clone()));
        }
        if let Some(height) = moment.height {
            env.insert("height".to_owned(), Value::from(height));
        }
        if let Some(width) = moment.width {
            env.insert("width".to_owned(), Value::from(width));
        }
        if let Some(md5) = moment.md5.as_ref() {
            env.insert("md5".to_owned(), Value::String(md5.clone()));
        }
        if let Some(file_size) = moment.file_size {
            env.insert("fileSize".to_owned(), Value::from(file_size));
        }
        if let Some(thumbnail) = moment.thumbnail.as_ref() {
            let mut thumb_env = serde_json::Map::new();
            if let Some(content_type) = thumbnail.content_type.as_ref() {
                thumb_env.insert(
                    "contentType".to_owned(),
                    Value::String(content_type.clone()),
                );
            }
            if let Some(md5) = thumbnail.md5.as_ref() {
                thumb_env.insert("md5".to_owned(), Value::String(md5.clone()));
            }
            if let Some(height) = thumbnail.height {
                thumb_env.insert("height".to_owned(), Value::from(height));
            }
            if let Some(width) = thumbnail.width {
                thumb_env.insert("width".to_owned(), Value::from(width));
            }
            if let Some(file_size) = thumbnail.file_size {
                thumb_env.insert("fileSize".to_owned(), Value::from(file_size));
            }
            if !thumb_env.is_empty() {
                env.insert("thumbnail".to_owned(), Value::Object(thumb_env));
            }
        }
        if !env.is_empty() {
            out.push(Value::Object(env));
        }
    }
    Value::Array(out)
}

pub(crate) fn parse_entry_put_response_bytes(bytes: &[u8]) -> Result<(Value, Option<Vec<u8>>)> {
    let Some(newline_idx) = bytes.iter().position(|b| *b == b'\n') else {
        let record: Value = serde_json::from_slice(bytes)
            .context("failed to parse entry PUT JSON response without payload")?;
        return Ok((record, None));
    };
    let line = &bytes[0..newline_idx];
    let record: Value =
        serde_json::from_slice(line).context("failed to parse entry PUT envelope JSON response")?;
    let payload = if let Some(content_len) = record.get("contentLength").and_then(Value::as_u64) {
        let payload_start = newline_idx + 1;
        let content_len_usize = usize::try_from(content_len)
            .context("entry PUT response contentLength exceeds usize")?;
        let payload_end = payload_start
            .checked_add(content_len_usize)
            .ok_or_else(|| anyhow!("entry PUT response payload length overflow"))?;
        if payload_end > bytes.len() {
            bail!("entry PUT response payload is truncated");
        }
        Some(bytes[payload_start..payload_end].to_vec())
    } else {
        None
    };
    Ok((record, payload))
}

/// Exponential backoff (1s, 2s, 4s, ... capped at `OUTBOX_MAX_ATTEMPTS`)
/// with bounded jitter of at most +/-25% of the base delay. The jitter is
/// derived by hashing `jitter_key` (the outbox item id) and the attempt
/// number rather than drawing from an rng, so retries of distinct items
/// spread out while the function stays deterministic for tests.
pub(crate) fn retry_delay_ms(attempt: i64, jitter_key: &str) -> i64 {
    let bounded = attempt.clamp(1, OUTBOX_MAX_ATTEMPTS);
    let base = 1_000 * (1_i64 << (bounded - 1));
    base + retry_jitter_offset_ms(base, jitter_key, bounded)
}

/// Deterministic jitter offset in `[-base/4, +base/4]`.
fn retry_jitter_offset_ms(base_delay_ms: i64, jitter_key: &str, attempt: i64) -> i64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let max_jitter = base_delay_ms / 4;
    if max_jitter <= 0 {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    jitter_key.hash(&mut hasher);
    attempt.hash(&mut hasher);
    let window = (max_jitter as u64) * 2 + 1;
    (hasher.finish() % window) as i64 - max_jitter
}

pub(crate) fn should_defer_outbox_item_until_keys(
    item: &OutboxItem,
    err: &OutboxProcessError,
) -> bool {
    if !resource_can_wait_for_encryption_keys(item) {
        return false;
    }
    match err {
        OutboxProcessError::Retryable(inner) => is_key_availability_error(inner),
        OutboxProcessError::NonRetryable { .. } => false,
    }
}

fn resource_can_wait_for_encryption_keys(item: &OutboxItem) -> bool {
    matches!(
        item.resource.as_str(),
        "daily_chat_feed" | "context_item" | "entry" | "original_media" | "journal" | "comment"
    )
}

fn is_key_availability_error(err: &anyhow::Error) -> bool {
    is_missing_active_content_public_key_error(err)
        || is_e2e_key_material_unavailable_error(err)
        || err.chain().any(|cause| {
            cause
                .downcast_ref::<StoreError>()
                .map(is_store_key_availability_error)
                .unwrap_or(false)
        })
}

fn is_store_key_availability_error(err: &StoreError) -> bool {
    matches!(
        err,
        StoreError::MissingKeychainMasterKey { .. }
            | StoreError::KeychainUnavailable(_)
            | StoreError::Keychain(_)
    )
}

pub(crate) fn mark_retry_or_failed<S: OutboxStore>(
    store: &S,
    item: &OutboxItem,
    err_text: &str,
) -> Result<(), String> {
    let next_attempt = item.attempt_count + 1;
    if next_attempt >= OUTBOX_MAX_ATTEMPTS {
        store
            .mark_outbox_item_failed(&item.id, &item.payload_json, next_attempt, Some(err_text))
            .map_err(|db_err| {
                format!(
                    "failed to mark outbox item failed id={} err={}",
                    item.id, db_err
                )
            })?;
        return Ok(());
    }

    let delay = retry_delay_ms(next_attempt, &item.id);
    store
        .mark_outbox_item_retry(
            &item.id,
            &item.payload_json,
            now_epoch_ms().saturating_add(delay),
            Some(err_text),
        )
        .map_err(|db_err| {
            format!(
                "failed to mark outbox item retry id={} err={}",
                item.id, db_err
            )
        })?;
    Ok(())
}

pub(crate) fn decode_entry_outbox_payload(
    item: &OutboxItem,
) -> OutboxProcessResult<EntryOutboxPayload> {
    serde_json::from_str(&item.payload_json).map_err(|err| OutboxProcessError::NonRetryable {
        reason: format!(
            "failed to decode entry outbox payload for item {}: {err}",
            item.id
        ),
    })
}

pub(crate) fn decode_comment_outbox_payload(
    item: &OutboxItem,
) -> OutboxProcessResult<CommentOutboxPayload> {
    serde_json::from_str(&item.payload_json).map_err(|err| OutboxProcessError::NonRetryable {
        reason: format!(
            "failed to decode comment outbox payload for item {}: {err}",
            item.id
        ),
    })
}

pub(crate) fn decode_original_media_outbox_payload(
    item: &OutboxItem,
) -> OutboxProcessResult<OriginalMediaOutboxPayload> {
    serde_json::from_str(&item.payload_json).map_err(|err| OutboxProcessError::NonRetryable {
        reason: format!(
            "failed to decode original_media outbox payload for item {}: {err}",
            item.id
        ),
    })
}

pub(crate) fn decode_context_item_outbox_payload(item: &OutboxItem) -> OutboxProcessResult<Value> {
    serde_json::from_str(&item.payload_json).map_err(|err| OutboxProcessError::NonRetryable {
        reason: format!(
            "failed to decode context_item outbox payload for item {}: {err}",
            item.id
        ),
    })
}

pub(crate) fn decode_daily_chat_feed_outbox_payload(
    item: &OutboxItem,
) -> OutboxProcessResult<Value> {
    serde_json::from_str(&item.payload_json).map_err(|err| OutboxProcessError::NonRetryable {
        reason: format!(
            "failed to decode daily_chat_feed outbox payload for item {}: {err}",
            item.id
        ),
    })
}

pub(crate) fn decode_daily_chat_settings_outbox_payload(
    item: &OutboxItem,
) -> OutboxProcessResult<Value> {
    serde_json::from_str(&item.payload_json).map_err(|err| OutboxProcessError::NonRetryable {
        reason: format!(
            "failed to decode daily_chat_settings outbox payload for item {}: {err}",
            item.id
        ),
    })
}

pub(crate) fn decode_journal_create_outbox_payload(
    item: &OutboxItem,
) -> OutboxProcessResult<Journal> {
    serde_json::from_str(&item.payload_json).map_err(|err| OutboxProcessError::NonRetryable {
        reason: format!(
            "failed to decode journal-create outbox payload for item {}: {err}",
            item.id
        ),
    })
}

pub(crate) fn reset_current_outbox_lease(store: &Store, item_id: &str) {
    let current_id = vec![item_id.to_owned()];
    if let Err(reset_err) = store.reset_processing_outbox_items_by_ids(&current_id) {
        log_sync(format!(
            "failed to reset current outbox lease id={} err={}",
            item_id, reset_err
        ));
    }
}

pub(crate) fn datetime_to_epoch_ms(dt: OffsetDateTime) -> i64 {
    let nanos = dt.unix_timestamp_nanos();
    let millis = nanos / 1_000_000;
    let clamped = millis.clamp(i64::MIN as i128, i64::MAX as i128);
    clamped as i64
}

pub(crate) fn entry_date_value_to_f64(date: Option<&Value>) -> Option<f64> {
    match date {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(raw)) => raw.parse::<f64>().ok().or_else(|| {
            OffsetDateTime::parse(raw, &Rfc3339)
                .ok()
                .map(|dt| datetime_to_epoch_ms(dt) as f64)
        }),
        _ => None,
    }
}

/// `Entry`'s serde definition accepts several spellings for some fields (e.g.
/// `user_edit_date` / `userEditDate`). When stored content carries more than
/// one spelling — server payload extras preserve camelCase keys while CLI
/// writes use snake_case — serde rejects the whole value with a
/// "duplicate field" error and the outbox item can never be pushed. Collapse
/// each alias group to a single canonical key, and coerce `moments: null`
/// (which serde's `default` does not cover) to an absent key.
pub(crate) fn normalize_entry_content_for_push(content: &mut Value) {
    const ALIAS_GROUPS: &[&[&str]] = &[
        &["journal_id", "journalId"],
        &["updated_at", "updatedAt"],
        &["deleted_at", "deletedAt"],
        &["user_edit_date", "userEditDate"],
        &["richTextJSON", "richTextJson", "rich_text_json"],
    ];
    let Some(obj) = content.as_object_mut() else {
        return;
    };
    for group in ALIAS_GROUPS {
        let present = group.iter().filter(|key| obj.contains_key(**key)).count();
        if present < 2 {
            continue;
        }
        // Prefer the first non-null value in group order; each group lists the
        // spelling `Entry` serializes (and the CLI maintains) first, e.g.
        // `user_edit_date` but `richTextJSON`.
        let kept = group
            .iter()
            .filter_map(|key| obj.get(*key))
            .find(|value| !value.is_null())
            .cloned();
        for key in *group {
            obj.remove(*key);
        }
        if let Some(value) = kept {
            obj.insert(group[0].to_owned(), value);
        }
    }
    if obj.get("moments").is_some_and(Value::is_null) {
        obj.remove("moments");
    }
}

pub(crate) fn resolve_outbox_edit_date_epoch_ms(
    payload: &EntryOutboxPayload,
    entry_content: &Entry,
    persisted_user_edit_date: Option<&str>,
) -> i64 {
    if let Some(user_edit_date) = persisted_user_edit_date
        && let Ok(dt) = OffsetDateTime::parse(user_edit_date, &Rfc3339)
    {
        return datetime_to_epoch_ms(dt);
    }
    queued_entry_edit_date_epoch_ms(payload, entry_content).unwrap_or_else(now_epoch_ms)
}

pub(crate) fn queued_entry_edit_date_epoch_ms(
    payload: &EntryOutboxPayload,
    entry_content: &Entry,
) -> Option<i64> {
    if let Some(ms) = payload.edit_date_epoch_ms {
        return Some(ms);
    }
    if let Some(ms) = payload.queued_at_epoch_ms {
        return Some(ms);
    }
    if let Some(ms) = entry_content
        .user_edit_date
        .as_ref()
        .and_then(Value::as_i64)
    {
        return Some(ms);
    }
    if let Some(queued_at) = payload.queued_at.as_deref()
        && let Ok(dt) = OffsetDateTime::parse(queued_at, &Rfc3339)
    {
        return Some(datetime_to_epoch_ms(dt));
    }
    if let Some(user_edit_date) = entry_content
        .user_edit_date
        .as_ref()
        .and_then(Value::as_str)
        && let Ok(dt) = OffsetDateTime::parse(user_edit_date, &Rfc3339)
    {
        return Some(datetime_to_epoch_ms(dt));
    }
    None
}

#[cfg(test)]
mod media_upload_tests {
    use super::*;
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::EntryAttachmentUpsert;
    use crate::sync::crypto::test_support;
    use rusqlite::params;
    use serde_json::json;
    use tempfile::Builder;

    const MEDIA_BYTES: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIF binary media bytes \x00\x01\x02\xff";
    const UPLOAD_BASE: &str = "https://upload.example/bucket";

    fn temp_store() -> (tempfile::TempDir, Store, i64) {
        let dir = Builder::new()
            .prefix("dayone-media-encrypt-")
            .tempdir()
            .expect("temp dir should create");
        let store = Store::open_at_with_in_memory_master_keys(dir.path().join("store.db"))
            .expect("store should open");
        let profile_id = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist")
            .id;
        seed_user_profile_upload_base(&store);
        (dir, store, profile_id)
    }

    fn seed_user_profile_upload_base(store: &Store) {
        let conn = store.connect().expect("connection should open");
        conn.execute(
            "INSERT INTO user_profile (id, data_json, updated_at) VALUES ('1', ?1, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET data_json = excluded.data_json",
            params![json!({ "sync_upload_base_url": UPLOAD_BASE }).to_string()],
        )
        .expect("user_profile should seed");
    }

    fn seed_user_keys(store: &Store, user_keys_json: &str) {
        let conn = store.connect().expect("connection should open");
        conn.execute(
            "INSERT INTO user_keys (id, data_json, updated_at) VALUES (1, ?1, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET data_json = excluded.data_json",
            params![user_keys_json],
        )
        .expect("user_keys should seed");
    }

    fn write_media_file(dir: &tempfile::TempDir) -> String {
        let path = dir.path().join("media.jpg");
        std::fs::write(&path, MEDIA_BYTES).expect("media file should write");
        path.to_string_lossy().into_owned()
    }

    fn seed_attachment(
        store: &Store,
        journal_id: &str,
        entry_id: &str,
        moment_id: &str,
        file_path: &str,
    ) {
        store
            .upsert_entry_attachment(&EntryAttachmentUpsert {
                journal_id: journal_id.to_owned(),
                entry_id: entry_id.to_owned(),
                moment_id: moment_id.to_owned(),
                file_path: file_path.to_owned(),
                moment_type: "image".to_owned(),
                content_type: "image/jpeg".to_owned(),
                file_size_bytes: MEDIA_BYTES.len() as i64,
                md5_body: format!("{:x}", Md5::digest(MEDIA_BYTES)),
                width: None,
                height: None,
                duration_seconds: None,
                thumbnail_content_type: None,
                thumbnail_md5: None,
                thumbnail_width: None,
                thumbnail_height: None,
                thumbnail_file_size_bytes: None,
                thumbnail_bytes: None,
            })
            .expect("attachment should upsert");
        store
            .mark_entry_attachments_associated(journal_id, entry_id)
            .expect("attachment association should mark");
    }

    fn media_outbox_item(journal_id: &str, entry_id: &str, moment_id: &str) -> OutboxItem {
        OutboxItem {
            id: format!("original_media:{journal_id}:{entry_id}:{moment_id}"),
            resource: "original_media".to_owned(),
            operation: "create".to_owned(),
            object_id: format!("{journal_id}:{entry_id}:{moment_id}"),
            payload_json: json!({
                "journal_id": journal_id,
                "entry_id": entry_id,
                "moment_id": moment_id
            })
            .to_string(),
            attempt_count: 0,
        }
    }

    fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[tokio::test]
    async fn encrypts_original_media_before_upload_for_e2e_journal() {
        let (dir, store, profile_id) = temp_store();
        let fixture = test_support::build_e2e_journal_fixture();
        let journal_id = "journal-e2e";

        store
            .upsert_json_row(
                "journals",
                journal_id,
                None,
                None,
                &fixture.journal.to_string(),
            )
            .expect("journal should seed");
        seed_user_keys(&store, &fixture.user_keys_json);
        store
            .set_encryption_key_for_profile(profile_id, &fixture.master_key)
            .expect("master key should set");

        let file_path = write_media_file(&dir);
        seed_attachment(&store, journal_id, "e1", "m1", &file_path);

        let upload_url = format!("{UPLOAD_BASE}/v2-{journal_id}-e1-m1");
        let api = FakeApiClient::new()
            .with_unit_fixture(format!("PUT_BYTES {upload_url}"))
            .await;

        let item = media_outbox_item(journal_id, "e1", "m1");
        push_original_media_outbox(&store, &api, profile_id, &item)
            .await
            .expect("encrypted media upload should succeed");

        let uploads = api.captured_uploads().await;
        assert_eq!(uploads.len(), 1, "exactly one PUT should happen");
        let upload = &uploads[0];
        assert_eq!(upload.url, upload_url);
        assert_eq!(upload.content_type, "application/octet-stream");

        // The server only ever sees ciphertext (a D1 blob), never the plaintext.
        assert!(upload.body.starts_with(b"D1"), "body should be a D1 blob");
        assert_ne!(upload.body, MEDIA_BYTES, "body must not be plaintext");
        assert!(
            !upload
                .body
                .windows(MEDIA_BYTES.len())
                .any(|window| window == MEDIA_BYTES),
            "plaintext media bytes must not appear anywhere in the ciphertext"
        );

        // S3 object md5 metadata describes the uploaded ciphertext.
        assert_eq!(
            header_value(&upload.headers, "x-amz-meta-md5"),
            Some(format!("{:x}", Md5::digest(&upload.body)).as_str())
        );
        // Server-side encryption stays on as defense-in-depth.
        assert_eq!(
            header_value(&upload.headers, "x-amz-server-side-encryption"),
            Some("AES256")
        );

        // The ciphertext decrypts back to the original media with the journal key.
        let decrypted = test_support::decrypt_media_with_journal_key(
            &upload.body,
            &fixture.journal_private_key,
            &fixture.fingerprint_hex,
        );
        assert_eq!(decrypted, MEDIA_BYTES);

        let attachment = store
            .get_entry_attachment(journal_id, "e1", "m1")
            .expect("attachment lookup should succeed")
            .expect("attachment should exist");
        assert!(
            attachment.original_uploaded,
            "successful upload should flip original_uploaded"
        );
    }

    #[tokio::test]
    async fn uploads_plaintext_media_for_non_encrypted_journal() {
        let (dir, store, profile_id) = temp_store();
        let journal_id = "journal-plain";

        store
            .upsert_json_row(
                "journals",
                journal_id,
                None,
                None,
                &json!({ "id": journal_id }).to_string(),
            )
            .expect("journal should seed");

        let file_path = write_media_file(&dir);
        seed_attachment(&store, journal_id, "e1", "m1", &file_path);

        let upload_url = format!("{UPLOAD_BASE}/v2-{journal_id}-e1-m1");
        let api = FakeApiClient::new()
            .with_unit_fixture(format!("PUT_FILE {upload_url}"))
            .await;

        let item = media_outbox_item(journal_id, "e1", "m1");
        push_original_media_outbox(&store, &api, profile_id, &item)
            .await
            .expect("plaintext media upload should succeed");

        let uploads = api.captured_uploads().await;
        assert_eq!(uploads.len(), 1);
        let upload = &uploads[0];
        assert_eq!(upload.url, upload_url);
        // Plaintext journals stream the original file as-is.
        assert_eq!(upload.content_type, "image/jpeg");
        assert_eq!(upload.body, MEDIA_BYTES);
        assert_eq!(
            header_value(&upload.headers, "x-amz-meta-md5"),
            Some(format!("{:x}", Md5::digest(MEDIA_BYTES)).as_str())
        );
    }
}
