use super::crypto::decrypt_entry_d1_payload;
use super::*;
use crate::store::entries::json::rebuild_entry_content;

pub(super) async fn run_entries_feed_resource<C: DayOneApiClient>(
    api: &C,
    store: &Store,
    run_id: i64,
    resources: &mut Vec<ResourceSyncOutput>,
    params: EntriesFeedResourceParams<'_>,
) {
    let EntriesFeedResourceParams {
        resource_key,
        api_path,
        static_params,
        entries_decryptor,
        embeddings_enabled,
    } = params;
    log_sync(format!("resource start kind=entries key={resource_key}"));
    let started = now_epoch_ms();
    let cursor_before = store.get_sync_cursor(resource_key).ok().flatten();
    let mut cursor = cursor_before.clone().unwrap_or_default();
    let mut changed_count = 0_i64;
    let mut status = "success".to_owned();
    let mut error: Option<String> = None;
    let mut loops = 0usize;
    loop {
        loops += 1;
        if loops > MAX_PAGE_LOOPS {
            status = "error".to_owned();
            error = Some("max page loops exceeded".to_owned());
            break;
        }

        let mut query: Vec<(&str, String)> = Vec::new();
        if !cursor.is_empty() {
            query.push(("cursor", cursor.clone()));
        }
        for (k, v) in static_params {
            query.push((k.as_str(), v.clone()));
        }

        let bytes = match api.get_bytes(api_path, &query).await {
            Ok(v) => v,
            Err(err) => {
                if is_deleted_journal_entries_feed_error(&err) {
                    log_sync(format!(
                        "entries feed skipped for deleted journal key={resource_key}: {err}"
                    ));
                    break;
                }
                status = "error".to_owned();
                error = Some(err.to_string());
                break;
            }
        };
        let records = match parse_entries_feed_bytes(&bytes) {
            Ok(v) => v,
            Err(err) => {
                status = "error".to_owned();
                error = Some(err.to_string());
                break;
            }
        };
        let page_items = records.len();
        if records.is_empty() {
            break;
        }

        // Decrypt every encrypted record before applying any page writes. If a
        // verified key is unavailable, retain the cursor and retry the page as
        // one unit instead of exposing a partially applied page.
        let mut records = records;
        let encrypted_error = records.iter_mut().find_map(|record| {
            let payload = record.payload_bytes.as_ref()?;
            if !payload.starts_with(b"D1") {
                return None;
            }
            let entry_id = entry_record_id(&record.record).unwrap_or_else(|| "unknown".to_owned());
            merge_entry_payload(&mut record.record, payload, entries_decryptor)
                .err()
                .map(|err| (entry_id, err))
        });
        if let Some((entry_id, err)) = encrypted_error {
            log_sync(format!("entry payload merge failed id={entry_id}: {err}"));
            status = "error".to_owned();
            error = Some(format!(
                "encrypted entry payload unavailable id={entry_id}: {err}"
            ));
            break;
        }

        let mut next_cursor = cursor.clone();
        for mut record in records {
            if let Some(c) = record_cursor(&record.record) {
                next_cursor = c;
            }
            let Some(entry_id) = entry_record_id(&record.record) else {
                continue;
            };
            if let Some(payload) = record.payload_bytes.as_ref()
                && !payload.starts_with(b"D1")
                && let Err(err) =
                    merge_entry_payload(&mut record.record, payload, entries_decryptor)
            {
                log_sync(format!("entry payload merge failed id={entry_id}: {err}"));
            }
            let entry_journal_id = record
                .record
                .journal_id
                .clone()
                .or_else(|| {
                    record
                        .record
                        .extra
                        .get("revision")
                        .and_then(|r| r.get("journalId"))
                        .map(value_to_string)
                        .map(JournalId::from)
                })
                .or_else(|| {
                    static_params
                        .iter()
                        .find(|(k, _)| k == "groups")
                        .map(|(_, v)| JournalId::from(v.as_str()))
                })
                .unwrap_or_default();
            if entry_journal_id.is_empty() {
                continue;
            }
            let updated_at = record.record.updated_at.as_deref().or_else(|| {
                record
                    .record
                    .extra
                    .get("updated_at")
                    .and_then(Value::as_str)
            });
            let deleted_at_from_revision =
                record.record.extra.get("revision").and_then(|revision| {
                    let is_delete = revision
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value == "delete");
                    is_delete.then(|| {
                        revision
                            .get("deletionRequested")
                            .or_else(|| revision.get("editDate"))
                            .map(value_to_string)
                    })?
                });
            let deleted_at = record
                .record
                .deleted_at
                .as_deref()
                .or_else(|| {
                    record
                        .record
                        .extra
                        .get("deleted_at")
                        .and_then(Value::as_str)
                })
                .or(deleted_at_from_revision.as_deref());
            let server_value = match entry_to_value(&record.record) {
                Ok(value) => value,
                Err(err) => {
                    status = "error".to_owned();
                    error = Some(err.to_string());
                    break;
                }
            };
            match queue_local_entry_update_if_newer(
                store,
                &entry_journal_id,
                &entry_id,
                &server_value,
            ) {
                Ok(true) => {
                    changed_count += 1;
                    continue;
                }
                Ok(false) => {}
                Err(err) => {
                    status = "error".to_owned();
                    error = Some(err.to_string());
                    break;
                }
            }
            let record_json = match serde_json::to_string(&record.record) {
                Ok(v) => v,
                Err(err) => {
                    status = "error".to_owned();
                    error = Some(err.to_string());
                    break;
                }
            };
            if deleted_at.is_some() {
                if let Err(err) = store.delete_local_entry(&entry_journal_id, &entry_id) {
                    status = "error".to_owned();
                    error = Some(err.to_string());
                    break;
                }
                changed_count += 1;
                continue;
            }
            if let Err(err) = store.upsert_entry_json_row(
                &entry_id,
                &entry_journal_id,
                updated_at,
                deleted_at,
                &record_json,
            ) {
                status = "error".to_owned();
                error = Some(err.to_string());
                break;
            }
            if let Err(err) =
                store.delete_settled_entry_outbox_for_entry(&entry_journal_id, &entry_id)
            {
                status = "error".to_owned();
                error = Some(err.to_string());
                break;
            }
            if *embeddings_enabled {
                let record_value = match entry_to_value(&record.record) {
                    Ok(value) => value,
                    Err(err) => {
                        log_sync(format!(
                            "entry serialization failed before embedding id={entry_id}: {err}"
                        ));
                        continue;
                    }
                };
                let searchable_text = entry_searchable_text(&record_value);
                let Some(computed_hash) = source_hash_for_searchable_text(&searchable_text) else {
                    continue;
                };
                let existing_hash = match store.get_entry_embedding_source_hash(&entry_id) {
                    Ok(value) => value,
                    Err(err) => {
                        log_sync(format!(
                            "entry embedding hash lookup failed id={entry_id}: {err}"
                        ));
                        None
                    }
                };
                if existing_hash.as_deref() == Some(computed_hash.as_str()) {
                    continue;
                }
                match compute_embedding_for_searchable_text(&searchable_text) {
                    Ok(Some(embedding)) => {
                        if let Err(err) = store.upsert_entry_embedding(
                            &entry_id,
                            &entry_journal_id,
                            &computed_hash,
                            &embedding,
                        ) {
                            log_sync(format!(
                                "entry embedding upsert failed id={entry_id}: {err}"
                            ));
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        log_sync(format!(
                            "entry embedding generation failed id={entry_id}: {err}; disabling embedding generation for the remaining entries in this sync run"
                        ));
                        *embeddings_enabled = false;
                    }
                }
            }
            changed_count += 1;
        }
        if status == "error" {
            break;
        }
        log_sync(format!(
            "resource page key={} items={} cursor_advanced={} changed_count_total={}",
            resource_key,
            page_items,
            next_cursor != cursor,
            changed_count
        ));
        if next_cursor != cursor {
            let _ = store.set_sync_cursor(resource_key, Some(&next_cursor));
            cursor = next_cursor;
            continue;
        }
        break;
    }

    if status == "success" {
        let _ = store.set_sync_resource_state_success(resource_key);
    } else if let Some(ref err) = error {
        let _ = store.set_sync_resource_state_error(resource_key, err);
    }

    let cursor_after = if cursor.is_empty() {
        None
    } else {
        Some(cursor.clone())
    };
    let cursor_advanced =
        cursor_before.as_deref().unwrap_or("") != cursor_after.as_deref().unwrap_or("");
    let finished = now_epoch_ms();
    resources.push(ResourceSyncOutput {
        resource: resource_key.to_owned(),
        started_at_epoch_ms: started,
        finished_at_epoch_ms: finished,
        changed_count,
        cursor_advanced,
        status: status.clone(),
        error: error.clone(),
    });
    let _ = store.save_sync_resource_run(SyncResourceRunSave {
        run_id,
        resource_key,
        changed_count,
        cursor_before: cursor_before.as_deref(),
        cursor_after: cursor_after.as_deref(),
        status: &status,
        error: error.as_deref(),
    });
    log_sync(format!(
        "resource done kind=entries key={} status={} changed_count={} cursor_advanced={}",
        resource_key, status, changed_count, cursor_advanced
    ));
}

fn entry_to_value(entry: &Entry) -> Result<Value> {
    Ok(serde_json::to_value(entry)?)
}

fn queue_local_entry_update_if_newer(
    store: &Store,
    journal_id: &str,
    entry_id: &str,
    server_value: &Value,
) -> Result<bool> {
    let Some((local_json, extra_fields_json)) =
        store.get_entry_json_row_with_extra_fields(entry_id)?
    else {
        return Ok(false);
    };
    let local_value: Value = serde_json::from_str(&local_json)?;
    let local_for_conflict =
        rebuild_entry_content(&local_value, extra_fields_json.as_deref(), entry_id);
    // Compare timestamps on a scratch copy: injecting the persisted
    // `user_edit_date` next to a preserved camelCase `userEditDate` would make
    // the snapshot fail `Entry` deserialization at push time with a serde
    // "duplicate field" error, so the snapshot itself must stay as rebuilt.
    let mut comparison = local_for_conflict.clone();
    if let Some(user_edit_date) = store.get_entry_user_edit_date(entry_id)?
        && let Some(obj) = comparison.as_object_mut()
        && !obj.contains_key("userEditDate")
    {
        obj.insert("user_edit_date".to_owned(), Value::String(user_edit_date));
    }
    if !local_is_newer(&comparison, server_value) {
        let server_is_delete = entry_value_is_deleted(server_value);
        let server_is_newer = local_is_newer(server_value, &comparison);
        let preserve_inflight_update = !server_is_delete
            && !server_is_newer
            && store.has_inflight_entry_update(journal_id, entry_id)?;
        if !preserve_inflight_update {
            return Ok(false);
        }
    }

    // Only push back to the server when there is evidence the CLI authored
    // local changes (a live outbox trail). Without it the "newer" local row is
    // itself server-origin — e.g. an entry moved between journals shows up in
    // both feeds with different revision edit dates — and pushing it would
    // fabricate an update the user never made.
    if !store.has_outbox_evidence_for_entry(entry_id)? {
        log_sync(format!(
            "entry conflict kept newer local entry journal_id={journal_id} entry_id={entry_id}; skipped queueing (no local outbox evidence)"
        ));
        return Ok(true);
    }

    let local_is_deleted = entry_value_is_deleted(&comparison);
    let operation = if local_is_deleted { "delete" } else { "update" };
    let now_ms = now_epoch_ms();
    let queued_at = crate::util::now_rfc3339_utc();
    let edit_date_epoch_ms = crate::sync::conflicts::extract_edit_timestamp(&comparison)
        .and_then(|ts| i64::try_from(ts).ok());
    let outbox_payload = serde_json::json!({
        "journal_id": journal_id,
        "entry_id": entry_id,
        "entry_json": local_for_conflict,
        "edit_date_epoch_ms": edit_date_epoch_ms,
        "queued_at": queued_at,
        "queued_at_epoch_ms": now_ms
    });
    store.enqueue_outbox_item(
        &format!("entry:{journal_id}:{entry_id}"),
        "entry",
        operation,
        &format!("{journal_id}:{entry_id}"),
        &outbox_payload.to_string(),
        now_ms,
    )?;
    log_sync(format!(
        "entry conflict kept newer local entry journal_id={journal_id} entry_id={entry_id}; queued {operation}"
    ));
    Ok(true)
}

fn entry_value_is_deleted(value: &Value) -> bool {
    value
        .get("deleted_at")
        .or_else(|| value.get("deletedAt"))
        .is_some_and(|value| !value.is_null())
        || value
            .get("revision")
            .and_then(|revision| revision.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|value| value == "delete")
}

pub(crate) fn is_deleted_journal_entries_feed_error(err: &anyhow::Error) -> bool {
    crate::http::error_chain_contains_http_status_and_body(
        err.as_ref(),
        404,
        "Journal has been deleted",
    )
}

pub(crate) fn parse_entries_feed_bytes(bytes: &[u8]) -> Result<Vec<FeedRecordWithPayload>> {
    let mut out = Vec::new();
    let mut idx = 0usize;
    while idx < bytes.len() {
        let Some(rel_newline) = bytes[idx..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let line_end = idx + rel_newline;
        let line = &bytes[idx..line_end];
        idx = line_end + 1;
        if line.is_empty() {
            continue;
        }
        let raw_record: Value = serde_json::from_slice(line)?;
        let mut record: Entry =
            serde_json::from_value(normalize_entry_feed_record_value(raw_record))?;
        let payload = if let Some(content_len) = record
            .extra
            .get("contentLength")
            .and_then(Value::as_u64)
            .or_else(|| record.extra.get("content_length").and_then(Value::as_u64))
        {
            let content_len = content_len as usize;
            if idx + content_len > bytes.len() {
                bail!("truncated entries feed payload");
            }
            let payload = bytes[idx..idx + content_len].to_vec();
            idx += content_len;
            Some(payload)
        } else {
            None
        };
        if record.cursor.is_none() {
            record.cursor = record.extra.get("cursor").map(value_to_string);
        }
        out.push(FeedRecordWithPayload {
            record,
            payload_bytes: payload,
        });
    }
    Ok(out)
}

pub(crate) fn normalize_entry_feed_record_value(mut value: Value) -> Value {
    fn normalize_string_field(obj: &mut serde_json::Map<String, Value>, key: &str) {
        if let Some(v) = obj.get_mut(key)
            && let Value::Number(n) = v
        {
            *v = Value::String(n.to_string());
        }
    }

    if let Some(obj) = value.as_object_mut() {
        for key in [
            "id",
            "journal_id",
            "journalId",
            "updated_at",
            "updatedAt",
            "deleted_at",
            "deletedAt",
            "cursor",
        ] {
            normalize_string_field(obj, key);
        }

        if let Some(revision) = obj.get_mut("revision").and_then(Value::as_object_mut) {
            normalize_string_field(revision, "entryId");
            normalize_string_field(revision, "journalId");
        }
    }
    value
}

pub(crate) fn entry_record_id(record: &Entry) -> Option<String> {
    if !record.id.trim().is_empty() {
        return Some(record.id.as_ref().to_owned());
    }
    record
        .extra
        .get("revision")
        .and_then(|r| r.get("entryId"))
        .map(value_to_string)
}

pub(crate) fn record_cursor(record: &Entry) -> Option<String> {
    record
        .cursor
        .clone()
        .or_else(|| record.extra.get("cursor").map(value_to_string))
}

pub(crate) fn merge_entry_payload(
    record: &mut Entry,
    payload: &[u8],
    entries_decryptor: Option<&EntriesDecryptor>,
) -> Result<()> {
    if payload.starts_with(b"{") {
        let value: Value = match serde_json::from_slice(payload) {
            Ok(value) => value,
            Err(err) => bail!("entry payload looked like JSON but was not valid JSON: {err}"),
        };
        merge_payload_value(record, value);
        return Ok(());
    }
    if payload.starts_with(b"D1") {
        let decryptor = entries_decryptor
            .ok_or_else(|| anyhow!("encrypted entry payload has no verified journal key"))?;
        let started = std::time::Instant::now();
        let decrypted = match decrypt_entry_d1_payload(payload, decryptor) {
            Ok(decrypted) => {
                crate::diagnostics::record_crypto(
                    "entry_payload_decrypt",
                    "journal",
                    "success",
                    started.elapsed(),
                    None,
                );
                decrypted
            }
            Err(error) => {
                crate::diagnostics::record_crypto(
                    "entry_payload_decrypt",
                    "journal",
                    "error",
                    started.elapsed(),
                    Some(error.as_ref()),
                );
                return Err(error);
            }
        };
        let value: Value = match serde_json::from_slice(&decrypted) {
            Ok(value) => value,
            Err(err) => bail!("decrypted D1 entry payload was not valid JSON: {err}"),
        };

        merge_payload_value(record, value);
        return Ok(());
    }
    Ok(())
}

pub(crate) fn merge_payload_value(record: &mut Entry, payload: Value) {
    const KNOWN_PAYLOAD_KEYS: &[&str] = &[
        "id",
        "journal_id",
        "journalId",
        "updated_at",
        "updatedAt",
        "deleted_at",
        "deletedAt",
        "user_edit_date",
        "userEditDate",
        "date",
        "body",
        "richTextJSON",
        "richTextJson",
        "rich_text_json",
        "moments",
    ];
    if let Some(payload_obj) = payload.as_object() {
        for (k, v) in payload_obj {
            if !KNOWN_PAYLOAD_KEYS.contains(&k.as_str()) && !record.extra.contains_key(k) {
                record.extra.insert(k.clone(), v.clone());
            }
            match k.as_str() {
                "id" if record.id.is_empty() => record.id = EntryId::from(value_to_string(v)),
                "journal_id" | "journalId" => {
                    if record.journal_id.is_none() {
                        record.journal_id = Some(JournalId::from(value_to_string(v)));
                    }
                }
                "updated_at" | "updatedAt" => {
                    if record.updated_at.is_none() {
                        record.updated_at = v.as_str().map(ToOwned::to_owned);
                    }
                }
                "deleted_at" | "deletedAt" => {
                    if record.deleted_at.is_none() {
                        record.deleted_at = v.as_str().map(ToOwned::to_owned);
                    }
                }
                "user_edit_date" | "userEditDate" => {
                    if record.user_edit_date.is_none() && !v.is_null() {
                        record.user_edit_date = Some(v.clone());
                    }
                }
                "date" => {
                    if record.date.is_none() && !v.is_null() {
                        record.date = Some(v.clone());
                    }
                }
                "body" => {
                    if record.body.is_none() {
                        record.body = v.as_str().map(ToOwned::to_owned);
                    }
                }
                "richTextJSON" | "richTextJson" | "rich_text_json" => {
                    if record.rich_text_json.is_none() && !v.is_null() {
                        record.rich_text_json = Some(v.clone());
                    }
                }
                "moments" => {
                    if record.moments.is_empty()
                        && let Ok(parsed) = serde_json::from_value::<Vec<Moment>>(v.clone())
                    {
                        record.moments = parsed;
                    }
                }
                _ => {}
            }
        }
    }
    record.payload = Some(payload);
    if !record.extra.contains_key("title") {
        let title = record
            .body
            .as_deref()
            .and_then(|body| {
                body.lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(|line| {
                        if line.chars().count() > 120 {
                            line.chars().take(120).collect::<String>()
                        } else {
                            line.to_owned()
                        }
                    })
            })
            .unwrap_or_default();
        if !title.is_empty() {
            record
                .extra
                .insert("title".to_owned(), Value::String(title));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry_embeddings::source_hash_for_searchable_text;
    use crate::http::fake::FakeApiClient;
    use crate::store::sqlite::Store;
    use rusqlite::params;
    use serde_json::json;
    use std::ops::Deref;
    use std::path::PathBuf;
    use tempfile::{Builder, TempDir};

    struct TestStorePath {
        _dir: TempDir,
        path: PathBuf,
    }

    impl Deref for TestStorePath {
        type Target = PathBuf;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    fn test_store_path(name: &str) -> TestStorePath {
        let dir = Builder::new()
            .prefix(&format!("dayone-cli-sync-entries-tests-{name}-"))
            .tempdir()
            .expect("temp dir should be created");
        let path = dir.path().join("store.db");
        TestStorePath { _dir: dir, path }
    }

    fn single_line_feed_bytes(value: &Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(value).expect("feed value should serialize");
        bytes.push(b'\n');
        bytes
    }

    fn multi_line_feed_bytes(values: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            out.extend(serde_json::to_vec(value).expect("feed value should serialize"));
            out.push(b'\n');
        }
        out
    }

    fn single_line_feed_bytes_with_payload(value: &Value, payload: &[u8]) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(value).expect("feed value should serialize");
        bytes.push(b'\n');
        bytes.extend_from_slice(payload);
        bytes
    }

    fn feed_bytes_with_payloads(records: &[(&Value, Option<&[u8]>)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (record, payload) in records {
            bytes.extend(serde_json::to_vec(record).expect("feed value should serialize"));
            bytes.push(b'\n');
            if let Some(payload) = payload {
                bytes.extend_from_slice(payload);
            }
        }
        bytes
    }

    #[tokio::test]
    async fn entries_feed_legacy_malformed_payload_does_not_block_sync_progress() {
        let path = test_store_path("feed-legacy-malformed-payload-progress");
        let store = Store::open_at(&*path).expect("store should open");
        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let payload = b"{not valid json";
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes_with_payload(
                    &json!({
                        "cursor": 1_712_757_600_000_i64,
                        "revision": {
                            "entryId": "entry-legacy-malformed-payload",
                            "type": "update",
                            "journalId": "journal-1",
                            "editDate": 1_712_757_500_000_i64,
                            "saveDate": 1_712_757_500_000_i64
                        },
                        "contentLength": payload.len(),
                        "encrypted": false
                    }),
                    payload,
                ),
            )
            .await
            .with_bytes_fixture("GET_BYTES /v2/sync/entries/journal-1/feed", Vec::new())
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].status, "success");
        assert_eq!(resources[0].changed_count, 1);
        assert!(
            store
                .get_entry_json_row_with_extra_fields("entry-legacy-malformed-payload")
                .expect("entry lookup should succeed")
                .is_some(),
            "legacy malformed payload should not prevent the feed record from being stored"
        );
        assert_eq!(
            store
                .get_sync_cursor("entries:journal-1")
                .expect("sync cursor lookup should succeed"),
            Some("1712757600000".to_owned()),
            "cursor should advance so a legacy malformed payload cannot block later sync work"
        );
    }

    #[tokio::test]
    async fn encrypted_payload_failure_preserves_local_content_and_cursor() {
        let path = test_store_path("encrypted-failure-preserves-local");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .set_sync_cursor("entries:journal-1", Some("previous-cursor"))
            .expect("existing cursor should save");
        store
            .upsert_entry_json_row(
                "entry-encrypted",
                "journal-1",
                Some("2026-04-10T00:00:00.000Z"),
                None,
                r#"{"id":"entry-encrypted","body":"readable local body"}"#,
            )
            .expect("local entry should save");
        let mut payload = vec![b'D', b'1', 1, 1];
        payload.extend_from_slice(&[0_u8; 32]); // unknown journal-key fingerprint
        payload.extend_from_slice(&[0_u8; 2 + 256 + 12 + 16 + 16]);
        let decryptor = EntriesDecryptor {
            journal_keys_by_fingerprint: HashMap::new(),
        };
        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let plaintext_record = json!({
            "cursor": 1_712_757_550_000_i64,
            "id": "entry-before-failure",
            "journal_id": "journal-1",
            "body": "must not be partially applied"
        });
        let encrypted_record = json!({
            "cursor": 1_712_757_600_000_i64,
            "revision": {
                "entryId": "entry-encrypted",
                "type": "update",
                "journalId": "journal-1",
                "editDate": 1_712_757_500_000_i64
            },
            "contentLength": payload.len(),
            "encrypted": true
        });
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                feed_bytes_with_payloads(&[
                    (&plaintext_record, None),
                    (&encrypted_record, Some(&payload)),
                ]),
            )
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: Some(&decryptor),
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        assert_eq!(resources[0].status, "error");
        assert_eq!(
            store
                .get_sync_cursor("entries:journal-1")
                .expect("cursor lookup should succeed")
                .as_deref(),
            Some("previous-cursor")
        );
        assert!(
            store
                .get_entry_json_row_with_extra_fields("entry-before-failure")
                .expect("entry lookup should succeed")
                .is_none(),
            "no records from a failed page should be applied"
        );
        let (stored, _) = store
            .get_entry_json_row_with_extra_fields("entry-encrypted")
            .expect("entry lookup should succeed")
            .expect("local entry should remain");
        assert_eq!(
            serde_json::from_str::<Value>(&stored)
                .expect("stored entry should parse")
                .get("body")
                .and_then(Value::as_str),
            Some("readable local body")
        );
    }

    #[tokio::test]
    async fn entries_feed_persists_server_revision_edit_date_as_user_edit_date() {
        let path = test_store_path("feed-revision-edit-date");
        let store = Store::open_at(&*path).expect("store should open");
        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_600_000_i64,
                    "revision": {
                        "entryId": "entry-from-feed",
                        "type": "update",
                        "journalId": "journal-1",
                        "editDate": 1_712_757_500_000_i64,
                        "saveDate": 1_712_757_500_000_i64
                    },
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        assert_eq!(
            store
                .get_entry_user_edit_date("entry-from-feed")
                .expect("entry edit date lookup should succeed")
                .as_deref(),
            Some("1712757500000")
        );
    }

    #[tokio::test]
    async fn entries_feed_preserves_newer_local_entry_against_older_server_revision_edit_date() {
        let path = test_store_path("feed-local-newer-conflict");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-conflict",
                "journal-1",
                Some("1712757600000"),
                Some("2024-04-10T12:00:00.000Z"),
                None,
                r#"{"id":"entry-conflict","body":"newer local body"}"#,
            )
            .expect("local entry should save");
        // Locally authored changes always leave an outbox trail; reconciliation
        // only keeps queueing pushes when such evidence exists.
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-conflict",
                "entry",
                "update",
                "journal-1:entry-conflict",
                r#"{"journal_id":"journal-1","entry_id":"entry-conflict"}"#,
                0,
            )
            .expect("pending update outbox should save");

        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_500_000_i64,
                    "revision": {
                        "entryId": "entry-conflict",
                        "type": "update",
                        "journalId": "journal-1",
                        "editDate": 1_712_757_500_000_i64,
                        "saveDate": 1_712_757_500_000_i64
                    },
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let (local_json, _) = store
            .get_entry_json_row_with_extra_fields("entry-conflict")
            .expect("entry lookup should succeed")
            .expect("local entry should remain");
        assert!(
            local_json.contains("newer local body"),
            "older server feed item should not overwrite newer local content"
        );
        let conn = store.connect().expect("connection should open");
        let outbox_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE resource = 'entry' AND operation = 'update' AND object_id = 'journal-1:entry-conflict' AND status = 'pending'",
                [],
                |row| row.get(0),
            )
            .expect("outbox query should succeed");
        assert_eq!(outbox_count, 1);
    }

    #[test]
    fn conflict_resolution_preserves_only_inflight_non_delete_ties() {
        let path = test_store_path("inflight-entry-update-tie");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-tie",
                "journal-1",
                Some("1712757600000"),
                Some("2024-04-10T12:00:00.000Z"),
                None,
                r#"{"id":"entry-tie","body":"pending local body"}"#,
            )
            .expect("local entry should save");
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-tie",
                "entry",
                "update",
                "journal-1:entry-tie",
                r#"{"journal_id":"journal-1","entry_id":"entry-tie"}"#,
                0,
            )
            .expect("pending update outbox should save");

        let tied_update = json!({
            "revision": { "type": "update", "editDate": 1_712_757_600_000_i64 }
        });
        assert!(
            queue_local_entry_update_if_newer(&store, "journal-1", "entry-tie", &tied_update)
                .expect("tie resolution should succeed"),
            "an in-flight local update must survive an edit-date tie"
        );
        assert_eq!(
            store
                .list_outbox_item_details(false)
                .expect("outbox should list")
                .len(),
            1,
            "tie resolution must not duplicate queued work"
        );
        let leased = store
            .lease_outbox_items(i64::MAX, 1)
            .expect("outbox lease should succeed");
        assert_eq!(leased.len(), 1);
        assert!(
            queue_local_entry_update_if_newer(&store, "journal-1", "entry-tie", &tied_update)
                .expect("processing tie resolution should succeed"),
            "an update being pushed must survive an edit-date tie"
        );
        assert_eq!(
            store
                .list_outbox_item_details(false)
                .expect("outbox should list")
                .len(),
            1,
            "processing tie resolution must not duplicate queued work"
        );

        let newer_update = json!({
            "revision": { "type": "update", "editDate": 1_712_757_600_001_i64 }
        });
        assert!(
            !queue_local_entry_update_if_newer(&store, "journal-1", "entry-tie", &newer_update)
                .expect("newer-server resolution should succeed"),
            "a strictly newer server update must still win"
        );

        let tied_delete = json!({
            "revision": { "type": "delete", "editDate": 1_712_757_600_000_i64 }
        });
        assert!(
            !queue_local_entry_update_if_newer(&store, "journal-1", "entry-tie", &tied_delete)
                .expect("delete resolution should succeed"),
            "a server tombstone must not be hidden by the tie guard"
        );

        store
            .upsert_entry_json_row_with_edit_date(
                "entry-without-outbox",
                "journal-1",
                Some("1712757600000"),
                None,
                None,
                r#"{"id":"entry-without-outbox","body":"server-origin body"}"#,
            )
            .expect("server-origin entry should save");
        assert!(
            !queue_local_entry_update_if_newer(
                &store,
                "journal-1",
                "entry-without-outbox",
                &tied_update,
            )
            .expect("server-origin tie resolution should succeed"),
            "a tie without queued local work must keep server-wins behavior"
        );
    }

    #[tokio::test]
    async fn entries_feed_conflict_snapshot_stays_deserializable_with_camel_user_edit_date() {
        // Server payloads carry a camelCase `userEditDate` that is deliberately
        // preserved in extra fields. The conflict snapshot must still
        // deserialize into `Entry` at push time (no duplicate alias keys).
        let path = test_store_path("feed-conflict-camel-ued");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-camel",
                "journal-1",
                Some("1712757600000"),
                Some("2024-04-10T12:00:00.000Z"),
                None,
                r#"{"id":"entry-camel","body":"newer local body","payload":{"body":"newer local body","userEditDate":1712757600000}}"#,
            )
            .expect("local entry should save");
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-camel",
                "entry",
                "update",
                "journal-1:entry-camel",
                r#"{"journal_id":"journal-1","entry_id":"entry-camel"}"#,
                0,
            )
            .expect("pending update outbox should save");

        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_500_000_i64,
                    "revision": {
                        "entryId": "entry-camel",
                        "type": "update",
                        "journalId": "journal-1",
                        "editDate": 1_712_757_500_000_i64
                    },
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await
            .with_bytes_fixture("GET_BYTES /v2/sync/entries/journal-1/feed", Vec::new())
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let conn = store.connect().expect("connection should open");
        let payload_json: String = conn
            .query_row(
                "SELECT payload_json FROM sync_outbox WHERE id = 'entry:journal-1:entry-camel'",
                [],
                |row| row.get(0),
            )
            .expect("conflict outbox item should exist");
        let payload: Value =
            serde_json::from_str(&payload_json).expect("outbox payload should parse");
        let entry_json = payload
            .get("entry_json")
            .cloned()
            .expect("conflict outbox payload should carry an entry snapshot");
        serde_json::from_value::<Entry>(entry_json.clone()).unwrap_or_else(|err| {
            panic!("conflict snapshot must deserialize for push, got '{err}': {entry_json}")
        });
    }

    #[tokio::test]
    async fn entries_feed_does_not_self_queue_update_without_local_outbox_evidence() {
        // A moved entry shows up in both the old and new journal feeds with
        // different revision edit dates. The older record makes the (purely
        // server-origin) local row look "newer"; reconciliation must keep the
        // local row but not fabricate a push for content the CLI never wrote.
        let path = test_store_path("feed-no-self-queue");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-moved",
                "journal-new",
                Some("1712757600000"),
                Some("2024-04-10T12:00:00.000Z"),
                None,
                r#"{"id":"entry-moved","body":"server body after move"}"#,
            )
            .expect("local entry should save");

        let static_params = vec![("groups".to_owned(), "journal-old".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-old/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_500_000_i64,
                    "revision": {
                        "entryId": "entry-moved",
                        "type": "update",
                        "journalId": "journal-old",
                        "editDate": 1_712_757_500_000_i64
                    },
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await
            .with_bytes_fixture("GET_BYTES /v2/sync/entries/journal-old/feed", Vec::new())
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-old",
                api_path: "/v2/sync/entries/journal-old/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let (local_json, _) = store
            .get_entry_json_row_with_extra_fields("entry-moved")
            .expect("entry lookup should succeed")
            .expect("local entry should remain");
        assert!(
            local_json.contains("server body after move"),
            "older feed record should not overwrite the newer local row"
        );
        let journal_id = store
            .get_entry_journal_id("entry-moved")
            .expect("journal lookup should succeed")
            .expect("entry should keep a journal");
        assert_eq!(
            journal_id, "journal-new",
            "older feed record should not flip the entry back to the old journal"
        );
        let conn = store.connect().expect("connection should open");
        let outbox_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row.get(0))
            .expect("outbox query should succeed");
        assert_eq!(
            outbox_count, 0,
            "reconciliation must not self-queue a push without local outbox evidence"
        );
    }

    #[tokio::test]
    async fn entries_feed_drops_older_local_update_when_newer_server_update_wins() {
        let path = test_store_path("feed-server-update-newer-than-update");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-server-update-wins",
                "journal-1",
                Some("1712757000000"),
                Some("2024-04-10T11:50:00.000Z"),
                None,
                r#"{"id":"entry-server-update-wins","body":"older local update"}"#,
            )
            .expect("local update should save");
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-server-update-wins",
                "entry",
                "update",
                "journal-1:entry-server-update-wins",
                r#"{"journal_id":"journal-1","entry_id":"entry-server-update-wins"}"#,
                0,
            )
            .expect("pending update outbox should save");

        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_800_000_i64,
                    "revision": {
                        "entryId": "entry-server-update-wins",
                        "type": "update",
                        "journalId": "journal-1",
                        "editDate": 1_712_757_800_000_i64
                    },
                    "body": "newer server update",
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await
            .with_bytes_fixture("GET_BYTES /v2/sync/entries/journal-1/feed", Vec::new())
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let (local_json, _) = store
            .get_entry_json_row_with_extra_fields("entry-server-update-wins")
            .expect("entry lookup should succeed")
            .expect("newer server update should remain");
        let local_value: Value =
            serde_json::from_str(&local_json).expect("stored entry should be valid json");
        assert_eq!(
            local_value.get("body").and_then(Value::as_str),
            Some("newer server update"),
            "newer server update should replace older local update body"
        );
        let conn = store.connect().expect("connection should open");
        let outbox_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE id = 'entry:journal-1:entry-server-update-wins'",
                [],
                |row| row.get(0),
            )
            .expect("outbox count should query");
        assert_eq!(
            outbox_count, 0,
            "newer server update should drop stale local update outbox"
        );
    }

    #[tokio::test]
    async fn entries_feed_preserves_newer_local_delete_against_older_server_update() {
        let path = test_store_path("feed-local-delete-newer-than-update");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-local-delete",
                "journal-1",
                Some("1712757800000"),
                Some("2024-04-10T12:03:20.000Z"),
                Some("2024-04-10T12:03:20.000Z"),
                r#"{"id":"entry-local-delete","body":"deleted local body","deleted_at":"2024-04-10T12:03:20.000Z"}"#,
            )
            .expect("local tombstone should save");
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-local-delete",
                "entry",
                "delete",
                "journal-1:entry-local-delete",
                r#"{"journal_id":"journal-1","entry_id":"entry-local-delete"}"#,
                0,
            )
            .expect("pending delete outbox should save");

        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_700_000_i64,
                    "revision": {
                        "entryId": "entry-local-delete",
                        "type": "update",
                        "journalId": "journal-1",
                        "editDate": 1_712_757_700_000_i64
                    },
                    "body": "older server body",
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await
            .with_bytes_fixture("GET_BYTES /v2/sync/entries/journal-1/feed", Vec::new())
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let (local_json, _) = store
            .get_entry_json_row_with_extra_fields("entry-local-delete")
            .expect("entry lookup should succeed")
            .expect("newer local tombstone should remain");
        let local_value: Value =
            serde_json::from_str(&local_json).expect("stored entry should be valid json");
        assert_eq!(
            local_value.get("body").and_then(Value::as_str),
            Some("deleted local body"),
            "older server update should not overwrite the newer local tombstone body"
        );
        assert_eq!(
            local_value.get("deleted_at").and_then(Value::as_str),
            Some("2024-04-10T12:03:20.000Z"),
            "older server update should not resurrect local tombstone"
        );
        let conn = store.connect().expect("connection should open");
        let operation: String = conn
            .query_row(
                "SELECT operation FROM sync_outbox WHERE id = 'entry:journal-1:entry-local-delete'",
                [],
                |row| row.get(0),
            )
            .expect("outbox operation should query");
        assert_eq!(
            operation, "delete",
            "newer local tombstone should keep a delete outbox item"
        );
    }

    #[tokio::test]
    async fn entries_feed_drops_older_local_delete_when_newer_server_update_wins() {
        let path = test_store_path("feed-server-update-newer-than-delete");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-server-update",
                "journal-1",
                Some("1712757000000"),
                Some("2024-04-10T11:50:00.000Z"),
                Some("2024-04-10T11:50:00.000Z"),
                r#"{"id":"entry-server-update","body":"older local delete","deleted_at":"2024-04-10T11:50:00.000Z"}"#,
            )
            .expect("local tombstone should save");
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-server-update",
                "entry",
                "delete",
                "journal-1:entry-server-update",
                r#"{"journal_id":"journal-1","entry_id":"entry-server-update"}"#,
                0,
            )
            .expect("pending delete outbox should save");

        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_800_000_i64,
                    "revision": {
                        "entryId": "entry-server-update",
                        "type": "update",
                        "journalId": "journal-1",
                        "editDate": 1_712_757_800_000_i64
                    },
                    "body": "newer server body",
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await
            .with_bytes_fixture("GET_BYTES /v2/sync/entries/journal-1/feed", Vec::new())
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let (local_json, extra_fields_json) = store
            .get_entry_json_row_with_extra_fields("entry-server-update")
            .expect("entry lookup should succeed")
            .expect("newer server update should remain");
        assert!(
            local_json.contains("newer server body"),
            "newer server update should replace older local tombstone"
        );
        assert!(
            !local_json.contains("deleted_at"),
            "newer server update should clear local tombstone"
        );
        assert!(
            !extra_fields_json
                .as_deref()
                .unwrap_or_default()
                .contains("deleted_at"),
            "newer server update should not leave stale tombstone data in extra fields"
        );
        let rebuilt = rebuild_entry_content(
            &serde_json::from_str(&local_json).expect("local entry should parse"),
            extra_fields_json.as_deref(),
            "entry-server-update",
        );
        assert!(
            rebuilt.get("deleted_at").is_none(),
            "rebuilt entry content should not contain stale tombstone data"
        );
        let conn = store.connect().expect("connection should open");
        let outbox_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE id = 'entry:journal-1:entry-server-update'",
                [],
                |row| row.get(0),
            )
            .expect("outbox count should query");
        assert_eq!(
            outbox_count, 0,
            "newer server update should drop stale local delete outbox"
        );
    }

    #[tokio::test]
    async fn entries_feed_applies_newer_remote_tombstone_by_deleting_local_entry() {
        let path = test_store_path("feed-remote-tombstone");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-delete",
                "journal-1",
                Some("1712757500000"),
                Some("2024-04-10T11:58:20.000Z"),
                None,
                r#"{"id":"entry-delete","body":"older local body"}"#,
            )
            .expect("local entry should save");
        store
            .enqueue_outbox_item(
                "entry:journal-1:entry-delete",
                "entry",
                "update",
                "journal-1:entry-delete",
                r#"{"journal_id":"journal-1","entry_id":"entry-delete"}"#,
                0,
            )
            .expect("local entry outbox should save");
        store
            .enqueue_outbox_item(
                "media:journal-1:entry-delete:m1",
                "original_media",
                "create",
                "journal-1:entry-delete:m1",
                r#"{"journal_id":"journal-1","entry_id":"entry-delete","moment_id":"m1"}"#,
                0,
            )
            .expect("local media outbox should save");
        store
            .enqueue_outbox_item(
                "comment:journal-1:entry-delete:c1",
                "comment",
                "update",
                "journal-1:entry-delete:c1",
                r#"{"journal_id":"journal-1","entry_id":"entry-delete","id":"c1"}"#,
                0,
            )
            .expect("local comment outbox should save");
        {
            let conn = store.connect().expect("connection should open");
            conn.execute(
                r#"INSERT INTO entry_attachments (journal_id, entry_id, moment_id, file_path, moment_type, content_type, file_size_bytes, md5_body)
                   VALUES (?1, ?2, ?3, ?4, 'image', 'image/jpeg', 123, 'md5')"#,
                params!["journal-1", "entry-delete", "m1", "/tmp/m1.jpg"],
            )
            .expect("attachment should save");
            conn.execute(
                r#"INSERT INTO comments (id, journal_id, entry_id, data_json) VALUES (?1, ?2, ?3, ?4)"#,
                params!["c1", "journal-1", "entry-delete", r#"{"id":"c1"}"#],
            )
            .expect("comment should save");
            conn.execute(
                r#"INSERT INTO comment_reactions (id, journal_id, entry_id, comment_id, user_id, reaction, data_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
                params![
                    "r1",
                    "journal-1",
                    "entry-delete",
                    "c1",
                    "u1",
                    "like",
                    r#"{"id":"r1"}"#
                ],
            )
            .expect("reaction should save");
        }

        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_700_000_i64,
                    "revision": {
                        "entryId": "entry-delete",
                        "type": "delete",
                        "journalId": "journal-1",
                        "editDate": 1_712_757_700_000_i64,
                        "deletionRequested": 1_712_757_700_000_i64
                    },
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await
            .with_bytes_fixture("GET_BYTES /v2/sync/entries/journal-1/feed", Vec::new())
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].status, "success");
        assert_eq!(resources[0].changed_count, 1);
        assert!(
            store
                .get_entry_json_row_with_extra_fields("entry-delete")
                .expect("entry lookup should succeed")
                .is_none(),
            "newer server tombstone should physically delete local entry"
        );
        let conn = store.connect().expect("connection should open");
        for table in ["entry_attachments", "comments", "comment_reactions"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("dependent count should query");
            assert_eq!(count, 0, "entry tombstone should delete {table}");
        }
        let outbox_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row.get(0))
            .expect("outbox count should query");
        assert_eq!(
            outbox_count, 0,
            "entry tombstone should drop stale entry/media/comment outbox"
        );
    }

    #[tokio::test]
    async fn entries_feed_preserves_newer_local_entry_against_older_remote_tombstone() {
        let path = test_store_path("feed-local-newer-than-tombstone");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-resurrect",
                "journal-1",
                Some("1712757800000"),
                Some("2024-04-10T12:03:20.000Z"),
                None,
                r#"{"id":"entry-resurrect","body":"newer local body"}"#,
            )
            .expect("local entry should save");
        let media_path = path.with_file_name("entry-resurrect-m1.jpg");
        std::fs::write(&media_path, b"local media bytes").expect("media file should save");
        store
            .enqueue_outbox_item(
                "media:journal-1:entry-resurrect:m1",
                "original_media",
                "create",
                "journal-1:entry-resurrect:m1",
                r#"{"journal_id":"journal-1","entry_id":"entry-resurrect","moment_id":"m1"}"#,
                0,
            )
            .expect("stale media outbox should save");
        {
            let conn = store.connect().expect("connection should open");
            conn.execute(
                r#"INSERT INTO entry_attachments (journal_id, entry_id, moment_id, file_path, moment_type, content_type, file_size_bytes, md5_body)
                   VALUES (?1, ?2, ?3, ?4, 'image', 'image/jpeg', 17, 'md5')"#,
                params![
                    "journal-1",
                    "entry-resurrect",
                    "m1",
                    media_path.to_string_lossy().as_ref()
                ],
            )
            .expect("attachment should save");
        }

        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = false;
        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "cursor": 1_712_757_700_000_i64,
                    "revision": {
                        "entryId": "entry-resurrect",
                        "type": "delete",
                        "journalId": "journal-1",
                        "editDate": 1_712_757_700_000_i64,
                        "deletionRequested": 1_712_757_700_000_i64
                    },
                    "contentLength": 0,
                    "encrypted": false
                })),
            )
            .await
            .with_bytes_fixture("GET_BYTES /v2/sync/entries/journal-1/feed", Vec::new())
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].status, "success");
        assert_eq!(resources[0].changed_count, 1);
        let (local_json, _) = store
            .get_entry_json_row_with_extra_fields("entry-resurrect")
            .expect("entry lookup should succeed")
            .expect("newer local entry should remain");
        assert!(
            local_json.contains("newer local body"),
            "older server tombstone should not overwrite newer local content"
        );
        let conn = store.connect().expect("connection should open");
        let entry_outbox_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE resource = 'entry' AND operation = 'update' AND object_id = 'journal-1:entry-resurrect' AND status = 'pending'",
                [],
                |row| row.get(0),
            )
            .expect("entry outbox query should succeed");
        assert_eq!(
            entry_outbox_count, 1,
            "newer local entry should be queued for resurrection"
        );
        let media_outbox_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE resource = 'original_media' AND object_id = 'journal-1:entry-resurrect:m1'",
                [],
                |row| row.get(0),
            )
            .expect("media outbox query should succeed");
        assert_eq!(
            media_outbox_count, 1,
            "older remote tombstone should keep pending media uploads for the resurrected local entry"
        );
        let attachment_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entry_attachments WHERE journal_id = 'journal-1' AND entry_id = 'entry-resurrect' AND moment_id = 'm1'",
                [],
                |row| row.get(0),
            )
            .expect("attachment count should query");
        assert_eq!(
            attachment_count, 1,
            "older remote tombstone should keep local media attachment metadata for resurrection"
        );
        assert_eq!(
            std::fs::read(&media_path).expect("media file should remain readable"),
            b"local media bytes",
            "older remote tombstone should leave local media file intact"
        );
    }

    #[tokio::test]
    async fn entries_feed_generates_embeddings_by_default() {
        let path = test_store_path("default-embedding");
        let store = Store::open_at(&*path).expect("store should open");
        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = true;

        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "id": "entry-1",
                    "journal_id": "journal-1",
                    "updated_at": "2026-04-10T00:00:00.000Z",
                    "body": "sync should embed this entry"
                })),
            )
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let hash = store
            .get_entry_embedding_source_hash("entry-1")
            .expect("embedding lookup should succeed");
        assert!(
            hash.is_some(),
            "entries feed should generate embeddings without a CLI toggle"
        );
    }

    #[tokio::test]
    async fn entries_feed_skips_embedding_upsert_when_source_hash_matches() {
        let path = test_store_path("hash-skip");
        let store = Store::open_at(&*path).expect("store should open");
        let static_params = vec![("groups".to_owned(), "journal-1".to_owned())];
        let mut embeddings_enabled = true;

        let source_hash = source_hash_for_searchable_text("hash-stable body")
            .expect("non-empty body should produce source hash");

        store
            .upsert_entry_embedding("entry-1", "journal-1", &source_hash, &[9.0, 9.0, 9.0])
            .expect("sentinel embedding should save");

        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-1/feed",
                single_line_feed_bytes(&json!({
                    "id": "entry-1",
                    "journal_id": "journal-1",
                    "updated_at": "2026-04-10T00:00:00.000Z",
                    "body": "hash-stable body"
                })),
            )
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            1,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-1",
                api_path: "/v2/sync/entries/journal-1/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let rows = store
            .list_entries_with_embeddings(Some("journal-1"), Some("entry-1"))
            .expect("entry listing should succeed");
        let (_, embedding) = rows.first().expect("entry row should exist");
        assert_eq!(
            embedding.as_deref(),
            Some(&[9.0, 9.0, 9.0][..]),
            "matching source hash should skip embedding overwrite"
        );
    }

    #[tokio::test]
    async fn entries_feed_skips_embeddings_when_embeddings_flag_is_disabled() {
        let path = test_store_path("embeddings-flag-disabled");
        let store = Store::open_at(&*path).expect("store should open");
        let static_params = vec![("groups".to_owned(), "journal-2".to_owned())];
        let mut embeddings_enabled = false;

        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-2/feed",
                single_line_feed_bytes(&json!({
                    "id": "entry-2",
                    "journal_id": "journal-2",
                    "updated_at": "2026-04-10T00:00:00.000Z",
                    "body": "embedding should be skipped because embeddings are disabled"
                })),
            )
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            2,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-2",
                api_path: "/v2/sync/entries/journal-2/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        let rows = store
            .list_entries_with_embeddings(Some("journal-2"), Some("entry-2"))
            .expect("entry listing should succeed");
        let (_, embedding) = rows.first().expect("entry row should exist");
        assert_eq!(
            embedding, &None,
            "disabled embeddings flag should skip embedding generation"
        );
    }

    #[tokio::test]
    async fn entries_feed_disables_embeddings_after_first_generation_error() {
        let path = test_store_path("disable-after-first-error");
        let store = Store::open_at(&*path).expect("store should open");
        let static_params = vec![("groups".to_owned(), "journal-3".to_owned())];
        let mut embeddings_enabled = true;

        let api = FakeApiClient::new()
            .with_bytes_fixture(
                "GET_BYTES /v2/sync/entries/journal-3/feed",
                multi_line_feed_bytes(&[
                    json!({
                        "id": "entry-a",
                        "journal_id": "journal-3",
                        "updated_at": "2026-04-10T00:00:00.000Z",
                        "body": "first entry triggers <<force-embedding-failure>>"
                    }),
                    json!({
                        "id": "entry-b",
                        "journal_id": "journal-3",
                        "updated_at": "2026-04-10T00:00:01.000Z",
                        "body": "second entry should be skipped after disable"
                    }),
                ]),
            )
            .await;

        let mut resources = Vec::new();
        run_entries_feed_resource(
            &api,
            &store,
            3,
            &mut resources,
            EntriesFeedResourceParams {
                resource_key: "entries:journal-3",
                api_path: "/v2/sync/entries/journal-3/feed",
                static_params: &static_params,
                entries_decryptor: None,
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;

        assert!(
            !embeddings_enabled,
            "embedding failure should disable embedding generation for the rest of the sync run"
        );
        let first_hash = store
            .get_entry_embedding_source_hash("entry-a")
            .expect("first embedding lookup should succeed");
        let second_hash = store
            .get_entry_embedding_source_hash("entry-b")
            .expect("second embedding lookup should succeed");
        assert_eq!(
            first_hash, None,
            "first entry should not persist an embedding"
        );
        assert_eq!(
            second_hash, None,
            "second entry should skip embedding after sync-run disable"
        );
    }
}
