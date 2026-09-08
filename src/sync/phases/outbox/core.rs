use super::*;
use crate::comment_feed::{CommentPageSyncError, CommentPageSyncRequest, sync_comment_pages};

pub(crate) async fn drain_sync_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    run_id: i64,
    profile_id: i64,
    resources: &mut Vec<ResourceSyncOutput>,
    resource_name: &str,
) -> Result<()> {
    let started = now_epoch_ms();
    let mut changed_count = 0_i64;
    let mut status = "success".to_owned();
    let mut error: Option<String> = None;
    let mut fatal_error: Option<String> = None;

    loop {
        let now = now_epoch_ms();
        let items = match store.lease_outbox_items(now, OUTBOX_BATCH_SIZE) {
            Ok(v) => v,
            Err(err) => {
                status = "error".to_owned();
                let msg = err.to_string();
                error = Some(msg.clone());
                fatal_error = Some(msg);
                break;
            }
        };
        if items.is_empty() {
            break;
        }
        let leased_ids: Vec<String> = items.iter().map(|it| it.id.clone()).collect();
        let mut processed_ids: Vec<String> = Vec::new();
        let mut restart_after_journal_create = false;
        let mut stop_processing = false;
        for item in items {
            processed_ids.push(item.id.clone());
            match process_outbox_item(store, api, profile_id, &item).await {
                Ok(()) => {
                    if let Err(db_err) =
                        store.mark_outbox_item_succeeded(&item.id, &item.payload_json)
                    {
                        let msg = format!(
                            "failed to mark outbox item succeeded id={} err={}",
                            item.id, db_err
                        );
                        log_sync(msg.clone());
                        let current_id = vec![item.id.clone()];
                        if let Err(reset_err) =
                            store.reset_processing_outbox_items_by_ids(&current_id)
                        {
                            log_sync(format!(
                                "failed to reset current outbox lease id={} err={}",
                                item.id, reset_err
                            ));
                        }
                        crate::diagnostics::record_outbox_result(
                            &item.resource,
                            &item.operation,
                            &item.object_id,
                            item.attempt_count.saturating_add(1),
                            "error",
                            Some(&db_err),
                        );
                        status = "error".to_owned();
                        if error.is_none() {
                            error = Some(msg);
                        }
                        if fatal_error.is_none() {
                            fatal_error = error.clone();
                        }
                        stop_processing = true;
                        break;
                    }
                    crate::diagnostics::record_outbox_result(
                        &item.resource,
                        &item.operation,
                        &item.object_id,
                        item.attempt_count.saturating_add(1),
                        "success",
                        None,
                    );
                    changed_count += 1;
                    if item.resource == "journal" && item.operation == "create" {
                        // Journal create can remap dependent entry outbox payloads; re-lease next batch.
                        restart_after_journal_create = true;
                        break;
                    }
                }
                Err(err) => {
                    let err_text = err.to_string();
                    let should_defer = should_defer_outbox_item_until_keys(&item, &err);
                    if should_defer {
                        let defer_attempt = item.attempt_count;
                        let defer_ms =
                            now_epoch_ms().saturating_add(OUTBOX_DEFER_MISSING_KEYS_DELAY_MS);
                        if let Err(db_err) = store.mark_outbox_item_deferred(
                            &item.id,
                            &item.payload_json,
                            defer_ms,
                            Some(&err_text),
                        ) {
                            let msg = format!(
                                "failed to defer outbox item retry id={} err={}",
                                item.id, db_err
                            );
                            log_sync(msg.clone());
                            status = "error".to_owned();
                            crate::diagnostics::record_outbox_result(
                                &item.resource,
                                &item.operation,
                                &item.object_id,
                                item.attempt_count.saturating_add(1),
                                "error",
                                Some(&db_err),
                            );
                            if error.is_none() {
                                error = Some(msg.clone());
                            }
                            if fatal_error.is_none() {
                                fatal_error = Some(msg);
                            }
                            reset_current_outbox_lease(store, &item.id);
                            stop_processing = true;
                            break;
                        }
                        crate::diagnostics::record_outbox_result(
                            &item.resource,
                            &item.operation,
                            &item.object_id,
                            item.attempt_count.saturating_add(1),
                            "deferred",
                            Some(&err),
                        );
                        status = "deferred".to_owned();
                        if error.is_none() {
                            error = Some(err_text.clone());
                        }
                        log_sync(format!(
                            "outbox item deferred id={} attempt={} err={}",
                            item.id, defer_attempt, err_text
                        ));
                        stop_processing = true;
                        break;
                    }

                    crate::diagnostics::record_outbox_result(
                        &item.resource,
                        &item.operation,
                        &item.object_id,
                        item.attempt_count.saturating_add(1),
                        "error",
                        Some(&err),
                    );
                    match &err {
                        OutboxProcessError::NonRetryable { .. } => {
                            if let Err(db_err) = store.mark_outbox_item_failed(
                                &item.id,
                                &item.payload_json,
                                OUTBOX_MAX_ATTEMPTS,
                                Some(&err_text),
                            ) {
                                let msg = format!(
                                    "failed to mark non-retryable outbox item failed id={} err={}",
                                    item.id, db_err
                                );
                                log_sync(msg.clone());
                                if error.is_none() {
                                    error = Some(msg);
                                }
                                if fatal_error.is_none() {
                                    fatal_error = error.clone();
                                }
                                reset_current_outbox_lease(store, &item.id);
                                stop_processing = true;
                                break;
                            }
                            log_sync(format!(
                                "outbox item permanently failed resource={} operation={} err={}",
                                item.resource, item.operation, err_text
                            ));
                            status = "error".to_owned();
                            if error.is_none() {
                                error = Some(format!(
                                    "outbox {} {} permanently failed: {}",
                                    item.resource, item.operation, err_text
                                ));
                            }
                            if fatal_error.is_none() {
                                fatal_error = error.clone();
                            }
                            stop_processing = true;
                            break;
                        }
                        OutboxProcessError::Retryable(_) => {}
                    }

                    let next_attempt = item.attempt_count + 1;
                    if let Err(msg) = mark_retry_or_failed(store, &item, &err_text) {
                        log_sync(msg.clone());
                        if error.is_none() {
                            error = Some(msg);
                        }
                        if fatal_error.is_none() {
                            fatal_error = error.clone();
                        }
                        reset_current_outbox_lease(store, &item.id);
                    }
                    if next_attempt >= OUTBOX_MAX_ATTEMPTS {
                        log_sync(format!(
                            "outbox item dead-lettered id={} attempts={} err={}",
                            item.id, next_attempt, err_text
                        ));
                        let msg = format!(
                            "outbox item dead-lettered id={} attempts={} err={}",
                            item.id, next_attempt, err_text
                        );
                        if error.is_none() {
                            error = Some(msg);
                        }
                    } else {
                        log_sync(format!(
                            "outbox item retry id={} attempt={} err={}",
                            item.id, next_attempt, err_text
                        ));
                    }
                    status = "error".to_owned();
                    if error.is_none() {
                        error = Some(format!(
                            "outbox push failed id={} err={}",
                            item.id, err_text
                        ));
                    }
                    if fatal_error.is_none() {
                        fatal_error = error.clone();
                    }
                    stop_processing = true;
                    break;
                }
            }
        }
        if stop_processing {
            let mut unprocessed_ids = Vec::new();
            for leased_id in &leased_ids {
                if !processed_ids.contains(leased_id) {
                    unprocessed_ids.push(leased_id.clone());
                }
            }
            if let Err(db_err) = store.reset_processing_outbox_items_by_ids(&unprocessed_ids) {
                let msg = format!("failed to reset unprocessed outbox leases err={db_err}");
                log_sync(msg.clone());
                status = "error".to_owned();
                if error.is_none() {
                    error = Some(msg.clone());
                }
                if fatal_error.is_none() {
                    fatal_error = Some(msg);
                }
            }
            break;
        }
        if restart_after_journal_create {
            let mut unprocessed_ids = Vec::new();
            for leased_id in &leased_ids {
                if !processed_ids.contains(leased_id) {
                    unprocessed_ids.push(leased_id.clone());
                }
            }
            if let Err(db_err) = store.reset_processing_outbox_items_by_ids(&unprocessed_ids) {
                let msg = format!("failed to reset unprocessed outbox leases err={db_err}");
                log_sync(msg.clone());
                status = "error".to_owned();
                if error.is_none() {
                    error = Some(msg.clone());
                }
                if fatal_error.is_none() {
                    fatal_error = Some(msg);
                }
                break;
            }
            continue;
        }
    }

    let finished = now_epoch_ms();
    resources.push(ResourceSyncOutput {
        resource: resource_name.to_owned(),
        started_at_epoch_ms: started,
        finished_at_epoch_ms: finished,
        changed_count,
        cursor_advanced: false,
        status: status.clone(),
        error: error.clone(),
    });
    let _ = store.save_sync_resource_run(SyncResourceRunSave {
        run_id,
        resource_key: resource_name,
        changed_count,
        cursor_before: None,
        cursor_after: None,
        status: &status,
        error: error.as_deref(),
    });
    if let Some(msg) = fatal_error {
        return Err(anyhow!(msg));
    }
    Ok(())
}

pub(crate) async fn process_outbox_item<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    log_sync(format!(
        "outbox processing id={} resource={} operation={} object={}",
        item.id, item.resource, item.operation, item.object_id
    ));
    match (item.resource.as_str(), item.operation.as_str()) {
        ("entry", "update") | ("entry", "delete") => {
            push_entry_outbox(store, api, profile_id, item).await
        }
        ("original_media", "create") => {
            push_original_media_outbox(store, api, profile_id, item).await
        }
        ("context_item", "update") => push_context_item_outbox(store, api, profile_id, item).await,
        ("daily_chat_feed", "update") => {
            push_daily_chat_feed_outbox(store, api, profile_id, item).await
        }
        ("daily_chat_settings", "update") => {
            push_daily_chat_settings_outbox(store, api, profile_id, item).await
        }
        ("comment", "create") | ("comment", "update") | ("comment", "delete") => {
            push_comment_outbox(store, api, profile_id, item).await
        }
        ("journal", "create") => push_journal_create_outbox(store, api, profile_id, item).await,
        ("journal", "update") => push_journal_update_outbox(store, api, profile_id, item).await,
        _ => {
            let msg = format!(
                "unknown outbox operation resource='{}' operation='{}' id={}",
                item.resource, item.operation, item.id
            );
            log_sync(msg.clone());
            Err(OutboxProcessError::NonRetryable { reason: msg })
        }
    }
}

pub(crate) async fn push_entry_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    let mut payload = decode_entry_outbox_payload(item)?;
    let journal_id = payload.journal_id.as_str();
    let entry_id = payload.entry_id.as_str();

    let persisted_user_edit_date = store.get_entry_user_edit_date(entry_id)?;
    let has_snapshot = payload.entry_json.is_some();
    let mut entry_content_value = if let Some(snapshot) = payload.entry_json.take() {
        snapshot
    } else {
        let (local_entry, extra_fields_json) = store
            .get_entry_json_row_with_extra_fields(entry_id)?
            .ok_or_else(|| OutboxProcessError::NonRetryable {
                reason: format!("entry '{}' missing locally for outbox push", entry_id),
            })?;
        let local_entry_value: Value = serde_json::from_str(&local_entry)?;
        build_entry_content_from_local(&local_entry_value, extra_fields_json.as_deref(), entry_id)
    };
    normalize_entry_content_for_push(&mut entry_content_value);
    let is_delete = item.operation == "delete";
    if !is_delete && let Some(obj) = entry_content_value.as_object_mut() {
        // A stray tombstone (e.g. left behind by a delete that was later
        // overwritten by `entry write`) must never ride along on an update:
        // it would re-delete the entry server-side and locally.
        obj.remove("deleted_at");
        obj.remove("deletedAt");
    }
    let entry_content: Entry = serde_json::from_value(entry_content_value.clone())
        .context("failed to deserialize rebuilt local entry content during outbox push")?;
    let entry_date = entry_date_value_to_f64(entry_content.date.as_ref())
        .or_else(|| {
            entry_content_value
                .get("date")
                .and_then(|value| entry_date_value_to_f64(Some(value)))
        })
        .unwrap_or(now_epoch_ms() as f64);
    let edit_date_epoch_ms = if has_snapshot {
        queued_entry_edit_date_epoch_ms(&payload, &entry_content).ok_or_else(|| OutboxProcessError::NonRetryable {
            reason: "queued entry snapshot has no saved edit timestamp; inspect dayone outbox list --payload before recovery".to_owned(),
        })?
    } else {
        resolve_outbox_edit_date_epoch_ms(
            &payload,
            &entry_content,
            persisted_user_edit_date.as_deref(),
        )
    };
    let envelope = if is_delete {
        json!({
            "entryId": entry_id,
            "type": "delete",
            "editDate": edit_date_epoch_ms as f64
        })
    } else {
        let envelope_moments = build_entry_envelope_moments(&entry_content.moments);
        json!({
            "entryId": entry_id,
            "type": "update",
            "entryDate": entry_date,
            "editDate": edit_date_epoch_ms as f64,
            "moments": envelope_moments
        })
    };
    let attachments = store.list_entry_attachments(journal_id, entry_id)?;
    let thumbnail_parts = build_entry_thumbnail_parts(&attachments);

    let journal_row = store
        .get_json_row_by_id("journals", journal_id)?
        .ok_or_else(|| anyhow!("journal '{}' missing locally for outbox push", journal_id))?;
    let journal: Journal = serde_json::from_str(&journal_row)?;
    let journal_value = serde_json::to_value(&journal)?;
    let (master_key, user_keys_json) = if journal_is_e2e_encrypted(&journal_value) {
        (
            store.get_encryption_key_for_profile(profile_id)?,
            store.get_user_identity_key_json()?,
        )
    } else {
        (None, None)
    };
    let content = encrypt_entry_content_for_journal(
        &journal_value,
        &entry_content_value,
        master_key.as_deref(),
        user_keys_json.as_deref(),
    )?;

    if item.operation == "update" {
        crate::sync::entry_lock::check_upload(
            store,
            api,
            profile_id,
            journal_id,
            entry_id,
            &journal_value,
        )
        .await?;
    }

    let response_bytes = match api
        .put_entry_multipart(
            &format!(
                "/v3/sync/entries/{}/{}",
                journal_id,
                encode_path_segment(entry_id)
            ),
            &serde_json::to_vec(&envelope)?,
            &content.field_name,
            &content.bytes,
            &content.mime_type,
            &thumbnail_parts,
        )
        .await
    {
        Ok(bytes) => bytes,
        Err(err)
            if is_journal_not_found_for_user_error(&err)
                && !store.has_active_local_journal(journal_id)? =>
        {
            log_sync(format!(
                "dropping stale entry outbox item id={} journal_id={} entry_id={} after journal-not-found response",
                item.id, journal_id, entry_id
            ));
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    let (record, _) = parse_entry_put_response_bytes(&response_bytes)?;
    validate_entry_put_outcome(&record, &item.operation)?;
    store.mark_entry_attachments_associated(journal_id, entry_id)?;

    let updated_local = entry_content_value;
    store.upsert_entry_json_row_with_edit_date(
        entry_id,
        journal_id,
        persisted_user_edit_date
            .as_deref()
            .or_else(|| {
                entry_content
                    .user_edit_date
                    .as_ref()
                    .and_then(Value::as_str)
            })
            .or(payload.queued_at.as_deref()),
        record.get("updated_at").and_then(Value::as_str),
        entry_content.deleted_at.as_deref(),
        &updated_local.to_string(),
    )?;
    Ok(())
}

/// Entry PUT reports both accepted and rejected writes with HTTP 200. Validate
/// its response envelope before the drain removes the local recovery payload.
fn validate_entry_put_outcome(record: &Value, operation: &str) -> OutboxProcessResult<()> {
    match record.get("outcome") {
        None => Ok(()),
        Some(Value::String(outcome)) if outcome == "clean" => Ok(()),
        Some(Value::String(outcome)) if outcome == "dirty" => {
            Err(OutboxProcessError::NonRetryable {
                reason: format!(
                    "server rejected entry {operation} (outcome=dirty); the local change was not persisted and remains available in the outbox"
                ),
            })
        }
        Some(_) => Err(OutboxProcessError::NonRetryable {
            reason: format!(
                "server returned an unrecognized outcome for entry {operation}; the local change was not confirmed and remains available in the outbox"
            ),
        }),
    }
}

fn is_journal_not_found_for_user_error(err: &anyhow::Error) -> bool {
    crate::http::error_chain_contains_http_status_and_body(
        err.as_ref(),
        404,
        "Journal not found for user",
    )
}

pub(crate) async fn push_context_item_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    let payload = decode_context_item_outbox_payload(item)?;
    let crypto_ctx = build_outbox_crypto_context(store, profile_id)?;
    let encrypted_payload = encrypt_context_library_payload(payload.clone(), &crypto_ctx)?;
    let (http_status, server_item) = api
        .put_json_allow_409("/labs/context-library/items", &encrypted_payload)
        .await?;
    let server_item_is_null = server_item.is_null();
    if http_status == 409 && server_item_is_null {
        return Err(OutboxProcessError::Retryable(anyhow!(
            "context item conflict returned empty body; cannot safely merge"
        )));
    }
    let final_item = if server_item_is_null {
        payload
    } else {
        decrypt_payload_owned(server_item, &crypto_ctx)?
    };
    let id = final_item
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("context item push response missing id"))?;
    let clear_needs_sync = http_status < 400 || !server_item_is_null;
    store.upsert_json_row_with_sync_state(
        "context_library_items",
        id,
        final_item.get("updated_at").and_then(Value::as_str),
        final_item.get("deleted_at").and_then(Value::as_str),
        Some(!clear_needs_sync),
        &final_item.to_string(),
    )?;
    if http_status == 409 {
        log_sync(format!("context item conflict merged id={id}"));
    }
    Ok(())
}

pub(crate) async fn push_daily_chat_feed_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    let payload = decode_daily_chat_feed_outbox_payload(item)?;
    let date = payload
        .get("date")
        .and_then(Value::as_str)
        .ok_or_else(|| OutboxProcessError::NonRetryable {
            reason: "daily_chat_feed outbox payload missing date".to_owned(),
        })?
        .to_owned();
    let crypto_ctx = build_outbox_crypto_context(store, profile_id)?;
    let encrypted = encrypt_payload(payload.clone(), &crypto_ctx)?;
    let path = format!("/labs/ai/daily-chat/{}", encode_path_segment(&date));
    let (http_status, server_payload) = api.put_json_allow_409(&path, &encrypted).await?;
    let server_payload_is_null = server_payload.is_null();
    if http_status == 409 && server_payload_is_null {
        return Err(OutboxProcessError::Retryable(anyhow!(
            "daily chat push conflict returned empty body for date={date}"
        )));
    }
    let final_payload = if server_payload_is_null {
        payload
    } else {
        decrypt_payload_owned(server_payload, &crypto_ctx)?
    };
    let final_id = final_payload
        .get("id")
        .map(value_to_string)
        .filter(|id: &String| !id.trim().is_empty())
        .unwrap_or_else(|| date.clone());
    let updated_at = final_payload.get("updated_at").and_then(Value::as_str);
    let deleted_at = final_payload.get("deleted_at").and_then(Value::as_str);
    store.upsert_json_row(
        "daily_chat_feed",
        &final_id,
        updated_at,
        deleted_at,
        &final_payload.to_string(),
    )?;
    if item.object_id != final_id {
        store.delete_json_row_by_id("daily_chat_feed", &item.object_id)?;
    }
    if http_status == 409 {
        log_sync(format!("daily chat conflict merged date={date}"));
    }
    Ok(())
}

pub(crate) async fn push_daily_chat_settings_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    let payload = decode_daily_chat_settings_outbox_payload(item)?;
    let crypto_ctx = build_outbox_crypto_context(store, profile_id)?;
    let encrypted = encrypt_payload(payload.clone(), &crypto_ctx)?;
    let (http_status, server_payload) = api
        .put_json_allow_409("/labs/ai/daily-chat/settings", &encrypted)
        .await?;
    let server_payload_is_null = server_payload.is_null();
    if http_status == 409 && server_payload_is_null {
        return Err(OutboxProcessError::Retryable(anyhow!(
            "daily chat settings push conflict returned empty body"
        )));
    }
    let final_payload = if server_payload_is_null {
        payload
    } else {
        decrypt_payload_owned(server_payload, &crypto_ctx)?
    };
    store.upsert_singleton_json_row(
        "daily_chat_settings",
        1,
        &final_payload.to_string(),
        final_payload.get("updated_at").and_then(Value::as_str),
    )?;
    if http_status == 409 {
        log_sync("daily chat settings conflict merged");
    }
    Ok(())
}

pub(crate) async fn push_journal_create_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    let local_payload = decode_journal_create_outbox_payload(item)?;
    let mut create_payload: serde_json::Map<String, Value> =
        local_payload.extra.clone().into_iter().collect();
    if !local_payload.id.is_empty() {
        create_payload.insert(
            "id".to_owned(),
            Value::String(local_payload.id.as_ref().to_owned()),
        );
    }
    if let Some(updated_at) = local_payload.updated_at.as_ref() {
        create_payload.insert("updated_at".to_owned(), Value::String(updated_at.clone()));
    }
    if let Some(deleted_at) = local_payload.deleted_at.as_ref() {
        create_payload.insert("deleted_at".to_owned(), Value::String(deleted_at.clone()));
    }
    let pending_id = create_payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let shared = create_payload
        .get("is_shared")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Remove local-only fields before sending to the server.
    create_payload.remove("pending_sync");
    create_payload.remove("is_shared");
    create_payload.remove("updated_at");
    normalize_legacy_description_for_journal_push(&mut create_payload);
    // Do not send synthetic local IDs on pending journal creates.
    if pending_id.starts_with("pending-") {
        create_payload.remove("id");
    }
    let path = if shared {
        "/shares"
    } else {
        "/v3/sync/journals"
    };
    // For an E2E journal, encrypt name/description_v2 under the vault key on the
    // way out only — the local row stays plaintext for display. Done at push
    // time (not in `journal create`) so the store mirrors how entry content is
    // handled: encrypted on the wire, never persisted encrypted locally.
    let plaintext_metadata_source = create_payload.clone();
    let metadata_crypto =
        JournalMetadataCrypto::load_for_payload(store, profile_id, &create_payload)?;
    metadata_crypto.encrypt_for_push(&mut create_payload)?;
    let created_encrypted = api.post_json(path, &Value::Object(create_payload)).await?;
    let server_id = created_encrypted
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("journal create response missing id"))?
        .to_owned();
    // The server echoes back the encrypted name/description we just sent; decrypt
    // them before persisting so the local display copy stays human-readable
    // (the pull path decrypts the same way). This is intentionally best-effort
    // after a successful POST: retrying here can create duplicate journals.
    let created_local = decrypt_journal_metadata_for_local_after_push(
        store,
        metadata_crypto.master_key(),
        created_encrypted,
        &plaintext_metadata_source,
    );
    store.upsert_json_row(
        "journals",
        &server_id,
        created_local.get("updated_at").and_then(Value::as_str),
        created_local.get("deleted_at").and_then(Value::as_str),
        &created_local.to_string(),
    )?;
    let server_id = server_id.as_str();
    if !pending_id.is_empty() && pending_id != server_id {
        // Adopt the server id everywhere the pending id was referenced (all
        // journal_id columns, the embeddings vec table, and the outbox).
        let remap = store.remap_journal_id(&pending_id, server_id)?;
        log_sync(format!(
            "reconciled journal id {pending_id} -> {server_id}: tables={:?} embeddings={} outbox={}",
            remap.tables, remap.embeddings_rebuilt, remap.outbox_items
        ));
        // journal_sync_info is excluded from the sweep; manage it explicitly.
        store.delete_journal_sync_info(&pending_id)?;
        store.delete_json_row_by_id("journals", &pending_id)?;
    }
    store.enable_journal_sync(server_id)?;
    Ok(())
}

pub(crate) async fn push_journal_update_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    let journal_id = item.object_id.as_str();
    // Re-read the local journal so we always push its latest state, regardless of
    // what was snapshotted into the outbox payload when the item was queued.
    let journal_row = store
        .get_json_row_by_id("journals", journal_id)?
        .ok_or_else(|| OutboxProcessError::NonRetryable {
            reason: format!("journal '{journal_id}' missing locally for update outbox push"),
        })?;
    let journal: Value = serde_json::from_str(&journal_row)?;

    let mut payload = build_journal_update_sync_payload(journal_id, &journal);
    // Encrypt name/description_v2 under the vault key on the wire only; the local
    // journal row (read above) stays plaintext for display.
    let plaintext_metadata_source = payload.clone();
    let metadata_crypto = JournalMetadataCrypto::load_for_payload(store, profile_id, &payload)?;
    metadata_crypto.encrypt_for_push(&mut payload)?;
    let path = format!("/v3/sync/journals/{journal_id}");
    let updated_encrypted = api.put_json(&path, &Value::Object(payload)).await?;

    // Persist the server's authoritative copy when it returns one, decrypting the
    // metadata it echoes back so the local display copy stays human-readable.
    if updated_encrypted
        .get("id")
        .and_then(Value::as_str)
        .is_some()
    {
        let updated_local = decrypt_journal_metadata_for_local_after_push(
            store,
            metadata_crypto.master_key(),
            updated_encrypted,
            &plaintext_metadata_source,
        );
        if !store.apply_journal_update_response_if_current(
            &item.id,
            &item.payload_json,
            journal_id,
            updated_local.get("updated_at").and_then(Value::as_str),
            updated_local.get("deleted_at").and_then(Value::as_str),
            &updated_local.to_string(),
        )? {
            log_sync("journal update response skipped: a newer local edit is queued");
        }
    }
    Ok(())
}

/// Profile-scoped crypto material needed to encrypt journal metadata for sync.
/// Keeping this as an explicit value avoids the weird "encrypt and return the
/// master key as a side quest" helper shape.
struct JournalMetadataCrypto {
    master_key: Option<String>,
    user_keys_json: Option<String>,
}

impl JournalMetadataCrypto {
    fn load_for_payload(
        store: &Store,
        profile_id: i64,
        payload: &serde_json::Map<String, Value>,
    ) -> OutboxProcessResult<Self> {
        if payload
            .get("encryption")
            .and_then(|value| value.get("vault"))
            .and_then(Value::as_object)
            .is_none()
        {
            return Ok(Self {
                master_key: None,
                user_keys_json: None,
            });
        }

        Ok(Self {
            master_key: store.get_encryption_key_for_profile(profile_id)?,
            user_keys_json: store.get_user_identity_key_json()?,
        })
    }

    fn encrypt_for_push(
        &self,
        payload: &mut serde_json::Map<String, Value>,
    ) -> OutboxProcessResult<()> {
        encrypt_journal_metadata_for_push(
            payload,
            self.master_key.as_deref(),
            self.user_keys_json.as_deref(),
        )?;
        Ok(())
    }

    fn master_key(&self) -> Option<&str> {
        self.master_key.as_deref()
    }
}

/// Normalizes the legacy/local `description` alias to the server's
/// `description_v2` field and ensures the alias never reaches the wire. Web keeps
/// journal descriptions locally as `description`, but sync sends `description_v2`.
fn normalize_legacy_description_for_journal_push(payload: &mut serde_json::Map<String, Value>) {
    if !payload.contains_key("description_v2")
        && let Some(description) = payload.remove("description")
    {
        payload.insert("description_v2".to_owned(), description);
        return;
    }
    payload.remove("description");
}

/// Decrypts a server journal response's `name`/`description_v2` back to plaintext
/// for local storage, mirroring the pull path's `try_decrypt_journal_fields`.
/// This runs after a successful push, so decryption is best-effort: a local
/// decrypt failure should not cause the outbox item to retry a POST/PUT that the
/// server already accepted. The local plaintext source is restored afterward so
/// partial echoes (or echoes without `encryption.vault`) cannot poison the local
/// display row with ciphertext.
fn decrypt_journal_metadata_for_local_after_push(
    store: &Store,
    master_key: Option<&str>,
    mut journal: Value,
    plaintext_source: &serde_json::Map<String, Value>,
) -> Value {
    if let Some(master_key) = master_key {
        match build_journal_decryptor(store, master_key, None) {
            Ok(Some(decryptor)) => {
                if let Err(err) = try_decrypt_journal_fields(&mut journal, &decryptor) {
                    log_sync(format!(
                        "journal metadata decrypt failed after accepted push; keeping local plaintext metadata: {err}"
                    ));
                }
            }
            Ok(None) => {}
            Err(err) => log_sync(format!(
                "journal metadata decryptor unavailable after accepted push; keeping local plaintext metadata: {err}"
            )),
        }
    }
    restore_plaintext_journal_metadata(&mut journal, plaintext_source);
    journal
}

fn restore_plaintext_journal_metadata(
    journal: &mut Value,
    plaintext_source: &serde_json::Map<String, Value>,
) {
    let Some(obj) = journal.as_object_mut() else {
        return;
    };
    if let Some(Value::String(name)) = plaintext_source.get("name") {
        obj.insert("name".to_owned(), Value::String(name.clone()));
    }
    if let Some(Value::String(description)) = plaintext_source
        .get("description_v2")
        .or_else(|| plaintext_source.get("description"))
    {
        obj.insert(
            "description_v2".to_owned(),
            Value::String(description.clone()),
        );
    }
    obj.remove("description");
}

/// Builds the journal-update request body the server expects (mirrors the Day
/// One web client's `toSync` whitelist). Only these fields are sent; server-
/// managed fields (participants, state, invite_list, ...) are intentionally
/// omitted. The `encryption` object is passed through verbatim from the local
/// journal, which is the seam used to push corrected E2E vaults.
fn build_journal_update_sync_payload(
    journal_id: &str,
    journal: &Value,
) -> serde_json::Map<String, Value> {
    // Journals store these flags as either booleans or 0/1 integers depending on
    // the source; coerce both to a real bool the way the web client's `!!x` does.
    let bool_field = |key: &str| match journal.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|v| v != 0)
            .or_else(|| n.as_f64().map(|v| v != 0.0))
            .unwrap_or(false),
        _ => false,
    };
    let mut out = serde_json::Map::new();

    out.insert("id".to_owned(), Value::String(journal_id.to_owned()));
    out.insert("kind".to_owned(), Value::String("standard".to_owned()));

    // Pass-through string/value fields when present.
    for key in ["name", "color", "sort_method", "template_id", "preset_id"] {
        if let Some(value) = journal.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    // The server's description field is `description_v2`; tolerate either local key.
    if let Some(desc) = journal
        .get("description_v2")
        .or_else(|| journal.get("description"))
    {
        out.insert("description_v2".to_owned(), desc.clone());
    }

    // Boolean flags, coerced like the web client does.
    for key in [
        "hide_all_entries",
        "hide_on_this_day",
        "hide_streaks",
        "conceal",
        "comments_disabled",
    ] {
        out.insert(key.to_owned(), Value::Bool(bool_field(key)));
    }
    // `add_location_to_new_entries` is the one journal flag that is natively a
    // boolean in the schema (the flags above are 0/1 integers, hence the
    // coercion). It needs no normalization, so pass it through verbatim — this
    // matches the web client, which only `!!`-coerces the integer flags.
    if let Some(value) = journal.get("add_location_to_new_entries") {
        out.insert("add_location_to_new_entries".to_owned(), value.clone());
    }

    let is_shared = bool_field("is_shared");
    out.insert("is_shared".to_owned(), Value::Bool(is_shared));
    if is_shared && let Some(perms) = journal.get("shared_permissions") {
        out.insert("shared_permissions".to_owned(), perms.clone());
    }
    if let Some(transfers) = journal.get("ownership_transfers") {
        out.insert("ownership_transfers".to_owned(), transfers.clone());
    }

    // Encryption vault (E2E) or the "plaintext" marker — pass through as-is.
    if let Some(encryption) = journal.get("encryption") {
        out.insert("encryption".to_owned(), encryption.clone());
    }

    out
}

pub(crate) async fn push_comment_outbox<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    item: &OutboxItem,
) -> OutboxProcessResult<()> {
    let payload = decode_comment_outbox_payload(item)?;
    let journal_id = payload.journal_id.as_str();
    let entry_id = payload.entry_id.as_str();
    let comment_id = payload.id.as_str();
    let journal_row = store
        .get_json_row_by_id("journals", journal_id)?
        .ok_or_else(|| OutboxProcessError::NonRetryable {
            reason: format!(
                "journal '{}' missing locally for comment outbox push",
                journal_id
            ),
        })?;
    let journal: Value = serde_json::from_str(&journal_row)?;
    match item.operation.as_str() {
        "create" => {
            let content =
                payload
                    .content
                    .as_deref()
                    .ok_or_else(|| OutboxProcessError::NonRetryable {
                        reason: "comment create payload missing content".to_owned(),
                    })?;
            let (master_key, user_keys_json) =
                comment_content_key_material(store, profile_id, &journal)?;
            let encoded = encode_comment_content_for_journal(
                &journal,
                content,
                master_key.as_deref(),
                user_keys_json.as_deref(),
            )?;
            let created = api
                .post_json(
                    &format!(
                        "/journals/{}/entries/{}/comments",
                        encode_path_segment(journal_id),
                        encode_path_segment(entry_id)
                    ),
                    &json!({ "content": encoded }),
                )
                .await?;
            let created_id = created
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| OutboxProcessError::NonRetryable {
                    reason: "comment create response missing valid id".to_owned(),
                })?;
            reconcile_comments_for_entry(
                store,
                api,
                journal_id,
                entry_id,
                &journal,
                Some((comment_id, created_id)),
            )
            .await?;
        }
        "update" => {
            let content =
                payload
                    .content
                    .as_deref()
                    .ok_or_else(|| OutboxProcessError::NonRetryable {
                        reason: "comment update payload missing content".to_owned(),
                    })?;
            let (master_key, user_keys_json) =
                comment_content_key_material(store, profile_id, &journal)?;
            let encoded = encode_comment_content_for_journal(
                &journal,
                content,
                master_key.as_deref(),
                user_keys_json.as_deref(),
            )?;
            let _ = api
                .put_json(
                    &format!(
                        "/journals/{}/entries/{}/comments/{}",
                        encode_path_segment(journal_id),
                        encode_path_segment(entry_id),
                        encode_path_segment(comment_id)
                    ),
                    &json!({ "content": encoded }),
                )
                .await?;
            reconcile_comments_for_entry(store, api, journal_id, entry_id, &journal, None).await?;
        }
        "delete" => {
            let _ = api
                .delete_json(
                    &format!(
                        "/journals/{}/entries/{}/comments/{}",
                        encode_path_segment(journal_id),
                        encode_path_segment(entry_id),
                        encode_path_segment(comment_id)
                    ),
                    &[],
                )
                .await?;
            reconcile_comments_for_entry(store, api, journal_id, entry_id, &journal, None).await?;
        }
        _ => {
            return Err(OutboxProcessError::NonRetryable {
                reason: format!("unsupported comment outbox operation '{}'", item.operation),
            });
        }
    }
    Ok(())
}

fn comment_content_key_material(
    store: &Store,
    profile_id: i64,
    journal: &Value,
) -> OutboxProcessResult<(Option<String>, Option<String>)> {
    if journal_is_e2e_encrypted(journal) {
        Ok((
            store.get_encryption_key_for_profile(profile_id)?,
            store.get_user_identity_key_json()?,
        ))
    } else {
        Ok((None, None))
    }
}

async fn reconcile_comments_for_entry<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    journal_id: &str,
    entry_id: &str,
    journal: &Value,
    pending_remap: Option<(&str, &str)>,
) -> OutboxProcessResult<()> {
    let cursor_key = Store::comments_cursor_key(journal_id, entry_id);
    let initial_cursor = if pending_remap.is_some() {
        // Create operations may require placeholder-ID reconciliation, so start this fetch
        // from the beginning in-memory. Keep the durable cursor unchanged so failed
        // reconciliations do not erase previously persisted progress.
        None
    } else {
        // Update/delete can usually continue from the existing cursor.
        store.get_sync_cursor(&cursor_key)?
    };
    let api_path = format!(
        "/journals/{}/entries/{}/comments",
        encode_path_segment(journal_id),
        encode_path_segment(entry_id)
    );
    let sync = sync_comment_pages(
        CommentPageSyncRequest {
            store,
            journal,
            journal_id,
            entry_id,
            cursor_key: &cursor_key,
            initial_cursor,
            context_label: "comment reconcile",
        },
        |cursor| async {
            let mut query = Vec::new();
            if let Some(cursor_value) = cursor {
                query.push(("cursor", cursor_value));
            }
            api.get_json(&api_path, &query).await
        },
    )
    .await;
    if let Err(err) = sync {
        return match err {
            CommentPageSyncError::Pagination(reason) => {
                Err(OutboxProcessError::NonRetryable { reason })
            }
            CommentPageSyncError::Other(source) => Err(OutboxProcessError::Retryable(source)),
        };
    }
    if let Some((from_id, to_id)) = pending_remap
        && !to_id.is_empty()
        && from_id != to_id
    {
        store.remap_comment_local_id(journal_id, entry_id, from_id, to_id)?;
    }
    Ok(())
}

#[cfg(test)]
mod journal_update_payload_tests {
    use super::build_journal_update_sync_payload;
    use serde_json::{Value, json};

    fn sample_journal() -> Value {
        json!({
            "id": "105442888546",
            "name": "DH",
            "description": "research notes",
            "color": "#E74C3C",
            "sort_method": "entryDate",
            "kind": "standard",
            "hide_all_entries": 0,
            "hide_on_this_day": false,
            "conceal": 1,
            "comments_disabled": true,
            "add_location_to_new_entries": true,
            "is_shared": false,
            "template_id": "",
            "encryption": { "vault": { "keys": [ {"fingerprint": "abc"} ] } },
            // Server-managed fields that must NOT be echoed back.
            "participants": [ {"id": "1"} ],
            "state": "active",
            "invite_list": { "active_invites": [] },
            "is_decrypted": 1,
            "owner_id": "247403433"
        })
    }

    #[test]
    fn includes_whitelisted_fields_and_passes_encryption_through() {
        let payload = build_journal_update_sync_payload("105442888546", &sample_journal());
        let obj = &payload;

        assert_eq!(obj.get("id").and_then(Value::as_str), Some("105442888546"));
        assert_eq!(obj.get("kind").and_then(Value::as_str), Some("standard"));
        assert_eq!(obj.get("name").and_then(Value::as_str), Some("DH"));
        assert_eq!(obj.get("color").and_then(Value::as_str), Some("#E74C3C"));
        // description -> description_v2 fallback
        assert_eq!(
            obj.get("description_v2").and_then(Value::as_str),
            Some("research notes")
        );
        // The encryption vault is passed through verbatim — the key-repair seam.
        assert_eq!(obj.get("encryption"), sample_journal().get("encryption"));
    }

    #[test]
    fn coerces_booleans_like_the_web_client() {
        let payload = build_journal_update_sync_payload("105442888546", &sample_journal());
        let obj = &payload;
        // 0/1 and true/false both normalize to real booleans.
        assert_eq!(obj.get("hide_all_entries"), Some(&Value::Bool(false)));
        assert_eq!(obj.get("conceal"), Some(&Value::Bool(true)));
        assert_eq!(obj.get("comments_disabled"), Some(&Value::Bool(true)));
        // Absent boolean (hide_streaks) defaults to false rather than being dropped.
        assert_eq!(obj.get("hide_streaks"), Some(&Value::Bool(false)));
    }

    #[test]
    fn omits_server_managed_fields() {
        let payload = build_journal_update_sync_payload("105442888546", &sample_journal());
        let obj = &payload;
        for forbidden in [
            "participants",
            "state",
            "invite_list",
            "is_decrypted",
            "owner_id",
        ] {
            assert!(
                obj.get(forbidden).is_none(),
                "server-managed field `{forbidden}` must not be echoed back"
            );
        }
    }

    #[test]
    fn shared_permissions_only_present_when_shared() {
        let mut journal = sample_journal();
        journal["shared_permissions"] = json!("creator_full+owner_delete");

        // Not shared: shared_permissions omitted.
        let payload = build_journal_update_sync_payload("j", &journal);
        assert!(payload.get("shared_permissions").is_none());

        // Shared: included.
        journal["is_shared"] = json!(true);
        let payload = build_journal_update_sync_payload("j", &journal);
        assert_eq!(
            payload.get("shared_permissions").and_then(Value::as_str),
            Some("creator_full+owner_delete")
        );
    }
}

/// End-to-end push tests for the journal-metadata encryption invariant: an E2E
/// journal's `name`/`description_v2` must travel the wire as D1 ciphertext while
/// the local row stays plaintext for display. Plaintext journals must be sent
/// (and stored) verbatim. These drive the real push functions against a
/// `FakeApiClient` that captures request bodies, and a real key hierarchy from
/// `build_e2e_journal_fixture`.
#[cfg(test)]
mod journal_metadata_push_tests {
    use super::*;
    use crate::http::fake::FakeApiClient;
    use crate::store::OutboxItem;
    use crate::store::sqlite::Store;
    use crate::sync::crypto::{
        encrypt_journal_metadata_for_push, test_support::build_e2e_journal_fixture,
    };
    use serde_json::{Value, json};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_db_path(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("dayone-cli-{label}-{unique}.db"))
    }

    /// A field is on the wire as encrypted D1 ciphertext, not its plaintext, and
    /// the captured wire value decrypts back to the exact original plaintext.
    fn assert_encrypted_on_wire(
        store: &Store,
        master_key: &str,
        encryption: &Value,
        body: &Value,
        field: &str,
        plaintext: &str,
    ) {
        let value = body
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("wire body should carry `{field}`"));
        assert!(
            value.starts_with("RDE"),
            "`{field}` must be sent as a D1 blob, got: {value}"
        );
        assert_ne!(
            value, plaintext,
            "`{field}` plaintext must never reach the wire"
        );

        let decryptor = build_journal_decryptor(store, master_key, None)
            .expect("journal decryptor should build")
            .expect("journal decryptor should be available");
        let mut journal = json!({
            "encryption": encryption,
            field: value,
        });
        try_decrypt_journal_fields(&mut journal, &decryptor)
            .expect("captured wire ciphertext should decrypt");
        assert_eq!(
            journal.get(field).and_then(Value::as_str),
            Some(plaintext),
            "captured `{field}` ciphertext should decrypt back to plaintext"
        );
    }

    /// Builds the encrypted copy of a journal the server echoes back after a
    /// create/update (it persists and returns the same D1 ciphertext it was
    /// sent), so the push path's decrypt-on-echo can be exercised.
    fn server_echo(
        base: &serde_json::Map<String, Value>,
        master_key: &str,
        user_keys_json: &str,
    ) -> Value {
        let mut echo = base.clone();
        encrypt_journal_metadata_for_push(&mut echo, Some(master_key), Some(user_keys_json))
            .expect("server echo should encrypt metadata");
        Value::Object(echo)
    }

    #[tokio::test]
    async fn push_journal_create_e2e_encrypts_wire_keeps_local_plaintext() {
        let fixture = build_e2e_journal_fixture();
        let encryption = fixture
            .journal
            .get("encryption")
            .cloned()
            .expect("fixture journal has encryption");
        let name = "Trip to Iceland";
        let description = "private travel notes";

        let store = Store::open_at_with_in_memory_master_keys(unique_db_path("journal-create-e2e"))
            .expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url("https://stg.dayone.me")
            .expect("profile should create");
        store
            .set_encryption_key_for_profile(profile.id, &fixture.master_key)
            .expect("encryption key should store");
        store
            .upsert_singleton_json_row("user_keys", 1, &fixture.user_keys_json, None)
            .expect("user keys should store");

        // The server echoes the ciphertext it was sent, plus the journal id.
        let echo_base: serde_json::Map<String, Value> = json!({
            "id": "journal-e2e",
            "name": name,
            "description_v2": description,
            "encryption": encryption,
        })
        .as_object()
        .expect("echo base is an object")
        .clone();
        let response = server_echo(&echo_base, &fixture.master_key, &fixture.user_keys_json);

        let payload_json = json!({
            "id": "journal-e2e",
            "name": name,
            "description_v2": description,
            "encryption": echo_base.get("encryption"),
        })
        .to_string();
        let item = OutboxItem {
            id: "journal:create:journal-e2e".to_owned(),
            resource: "journal".to_owned(),
            operation: "create".to_owned(),
            object_id: "journal-e2e".to_owned(),
            payload_json,
            attempt_count: 0,
        };

        let api = FakeApiClient::new()
            .with_json_fixture("POST_JSON /v3/sync/journals", response)
            .await;

        push_journal_create_outbox(&store, &api, profile.id, &item)
            .await
            .expect("create push should succeed");

        // Wire: name/description_v2 are D1 ciphertext, never plaintext.
        let bodies = api.captured_post_json_bodies("/v3/sync/journals").await;
        let sent = bodies.first().expect("create should POST one body");
        let sent_encryption = echo_base.get("encryption").expect("echo encryption");
        assert_encrypted_on_wire(
            &store,
            &fixture.master_key,
            sent_encryption,
            sent,
            "name",
            name,
        );
        assert_encrypted_on_wire(
            &store,
            &fixture.master_key,
            sent_encryption,
            sent,
            "description_v2",
            description,
        );

        // Local: the persisted row stays human-readable for display.
        let row = store
            .get_json_row_by_id("journals", "journal-e2e")
            .expect("journal row read should succeed")
            .expect("journal row should exist locally");
        let local: Value = serde_json::from_str(&row).expect("local row is json");
        assert_eq!(local.get("name").and_then(Value::as_str), Some(name));
        assert_eq!(
            local.get("description_v2").and_then(Value::as_str),
            Some(description)
        );
    }

    #[tokio::test]
    async fn push_journal_create_e2e_normalizes_legacy_description_and_handles_partial_echo() {
        let fixture = build_e2e_journal_fixture();
        let encryption = fixture
            .journal
            .get("encryption")
            .cloned()
            .expect("fixture journal has encryption");
        let name = "Legacy Description Journal";
        let description = "legacy description should still be private";

        let store = Store::open_at_with_in_memory_master_keys(unique_db_path(
            "journal-create-legacy-description",
        ))
        .expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url("https://stg.dayone.me")
            .expect("profile should create");
        store
            .set_encryption_key_for_profile(profile.id, &fixture.master_key)
            .expect("encryption key should store");
        store
            .upsert_singleton_json_row("user_keys", 1, &fixture.user_keys_json, None)
            .expect("user keys should store");

        let echo_base: serde_json::Map<String, Value> = json!({
            "id": "journal-e2e",
            "name": name,
            "description_v2": description,
            "encryption": encryption,
        })
        .as_object()
        .expect("echo base is an object")
        .clone();
        let mut response = server_echo(&echo_base, &fixture.master_key, &fixture.user_keys_json);
        response
            .as_object_mut()
            .expect("server echo should be an object")
            .remove("encryption");

        let payload_json = json!({
            "id": "journal-e2e",
            "name": name,
            // Raw JSON callers may still use the old/local `description` alias.
            // It must be normalized to encrypted `description_v2` before sync.
            "description": description,
            "encryption": echo_base.get("encryption"),
        })
        .to_string();
        let item = OutboxItem {
            id: "journal:create:journal-e2e".to_owned(),
            resource: "journal".to_owned(),
            operation: "create".to_owned(),
            object_id: "journal-e2e".to_owned(),
            payload_json,
            attempt_count: 0,
        };

        let api = FakeApiClient::new()
            .with_json_fixture("POST_JSON /v3/sync/journals", response)
            .await;

        push_journal_create_outbox(&store, &api, profile.id, &item)
            .await
            .expect("create push should succeed even when echo omits encryption");

        let bodies = api.captured_post_json_bodies("/v3/sync/journals").await;
        let sent = bodies.first().expect("create should POST one body");
        assert!(
            sent.get("description").is_none(),
            "legacy description alias must not reach the wire"
        );
        let sent_encryption = echo_base.get("encryption").expect("echo encryption");
        assert_encrypted_on_wire(
            &store,
            &fixture.master_key,
            sent_encryption,
            sent,
            "name",
            name,
        );
        assert_encrypted_on_wire(
            &store,
            &fixture.master_key,
            sent_encryption,
            sent,
            "description_v2",
            description,
        );

        // The partial echo lacks `encryption.vault`, so decrypting the echo alone
        // would be a no-op. Local metadata should still be restored from the
        // plaintext source instead of persisting ciphertext.
        let row = store
            .get_json_row_by_id("journals", "journal-e2e")
            .expect("journal row read should succeed")
            .expect("journal row should exist locally");
        let local: Value = serde_json::from_str(&row).expect("local row is json");
        assert_eq!(local.get("name").and_then(Value::as_str), Some(name));
        assert_eq!(
            local.get("description_v2").and_then(Value::as_str),
            Some(description)
        );
        assert!(
            local.get("description").is_none(),
            "legacy description alias should not be persisted after reconcile"
        );
    }

    #[tokio::test]
    async fn push_journal_update_e2e_encrypts_wire_keeps_local_plaintext() {
        let fixture = build_e2e_journal_fixture();
        let encryption = fixture
            .journal
            .get("encryption")
            .cloned()
            .expect("fixture journal has encryption");
        let name = "Renamed Vault";
        let description = "updated secret description";

        let store = Store::open_at_with_in_memory_master_keys(unique_db_path("journal-update-e2e"))
            .expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url("https://stg.dayone.me")
            .expect("profile should create");
        store
            .set_encryption_key_for_profile(profile.id, &fixture.master_key)
            .expect("encryption key should store");
        store
            .upsert_singleton_json_row("user_keys", 1, &fixture.user_keys_json, None)
            .expect("user keys should store");

        // Local journal row is plaintext (the display source of truth).
        let local_journal = json!({
            "id": "journal-e2e",
            "name": name,
            "description_v2": description,
            "encryption": encryption,
        });
        store
            .upsert_json_row(
                "journals",
                "journal-e2e",
                None,
                None,
                &local_journal.to_string(),
            )
            .expect("local journal should save");
        store
            .replace_journal_metadata_diagnostics("journal-e2e", &[("name", "plaintext")])
            .expect("stale diagnostic should save");

        let echo_base: serde_json::Map<String, Value> = json!({
            "id": "journal-e2e",
            "name": name,
            "description_v2": description,
            "encryption": local_journal.get("encryption"),
        })
        .as_object()
        .expect("echo base is an object")
        .clone();
        let response = server_echo(&echo_base, &fixture.master_key, &fixture.user_keys_json);

        let item = OutboxItem {
            id: "journal:update:journal-e2e".to_owned(),
            resource: "journal".to_owned(),
            operation: "update".to_owned(),
            object_id: "journal-e2e".to_owned(),
            payload_json: "{}".to_owned(),
            attempt_count: 0,
        };

        let api = FakeApiClient::new()
            .with_put_json_fixture("PUT_JSON /v3/sync/journals/journal-e2e", response)
            .await;

        push_journal_update_outbox(&store, &api, profile.id, &item)
            .await
            .expect("update push should succeed");

        // Wire: D1 ciphertext only.
        let bodies = api
            .captured_put_json_bodies("/v3/sync/journals/journal-e2e")
            .await;
        let sent = bodies.first().expect("update should PUT one body");
        let sent_encryption = local_journal.get("encryption").expect("local encryption");
        assert_encrypted_on_wire(
            &store,
            &fixture.master_key,
            sent_encryption,
            sent,
            "name",
            name,
        );
        assert_encrypted_on_wire(
            &store,
            &fixture.master_key,
            sent_encryption,
            sent,
            "description_v2",
            description,
        );

        // Local: still plaintext after the server's echoed copy is decrypted.
        let row = store
            .get_json_row_by_id("journals", "journal-e2e")
            .expect("journal row read should succeed")
            .expect("journal row should exist locally");
        let local: Value = serde_json::from_str(&row).expect("local row is json");
        assert_eq!(local.get("name").and_then(Value::as_str), Some(name));
        assert_eq!(
            local.get("description_v2").and_then(Value::as_str),
            Some(description)
        );
        assert_eq!(
            store
                .journal_metadata_diagnostics("journal-e2e")
                .expect("diagnostics should load"),
            vec![("name".to_owned(), "plaintext".to_owned())],
            "the finding remains unchanged until a later pull validates the raw server metadata"
        );
    }

    #[tokio::test]
    async fn push_journal_update_without_id_echo_leaves_local_row_untouched() {
        let fixture = build_e2e_journal_fixture();
        let encryption = fixture
            .journal
            .get("encryption")
            .cloned()
            .expect("fixture journal has encryption");
        let name = "Untouched Vault";
        let description = "must stay plaintext locally";

        let store =
            Store::open_at_with_in_memory_master_keys(unique_db_path("journal-update-no-echo"))
                .expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url("https://stg.dayone.me")
            .expect("profile should create");
        store
            .set_encryption_key_for_profile(profile.id, &fixture.master_key)
            .expect("encryption key should store");
        store
            .upsert_singleton_json_row("user_keys", 1, &fixture.user_keys_json, None)
            .expect("user keys should store");

        let local_journal = json!({
            "id": "journal-e2e",
            "name": name,
            "description_v2": description,
            "encryption": encryption,
        });
        store
            .upsert_json_row(
                "journals",
                "journal-e2e",
                None,
                None,
                &local_journal.to_string(),
            )
            .expect("local journal should save");
        let before = store
            .get_json_row_by_id("journals", "journal-e2e")
            .expect("journal row read should succeed")
            .expect("journal row should exist locally");

        let item = OutboxItem {
            id: "journal:update:journal-e2e".to_owned(),
            resource: "journal".to_owned(),
            operation: "update".to_owned(),
            object_id: "journal-e2e".to_owned(),
            payload_json: "{}".to_owned(),
            attempt_count: 0,
        };

        // Server returns a body with no `id` (the no-op echo guard path).
        let api = FakeApiClient::new()
            .with_put_json_fixture("PUT_JSON /v3/sync/journals/journal-e2e", json!({}))
            .await;

        push_journal_update_outbox(&store, &api, profile.id, &item)
            .await
            .expect("update push should succeed");

        // Wire still encrypts.
        let bodies = api
            .captured_put_json_bodies("/v3/sync/journals/journal-e2e")
            .await;
        let sent = bodies.first().expect("update should PUT one body");
        assert_encrypted_on_wire(
            &store,
            &fixture.master_key,
            local_journal.get("encryption").expect("local encryption"),
            sent,
            "name",
            name,
        );

        // Local row is byte-for-byte unchanged: no echo means no re-persist.
        let after = store
            .get_json_row_by_id("journals", "journal-e2e")
            .expect("journal row read should succeed")
            .expect("journal row should exist locally");
        assert_eq!(before, after, "no-id echo must not rewrite the local row");
    }

    #[tokio::test]
    async fn push_journal_create_plaintext_sends_and_stores_verbatim() {
        let name = "Visible Journal";

        let store =
            Store::open_at_with_in_memory_master_keys(unique_db_path("journal-create-plain"))
                .expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url("https://stg.dayone.me")
            .expect("profile should create");

        // Plaintext journal: no `encryption`, so nothing to encrypt.
        let payload_json = json!({
            "id": "journal-plain",
            "name": name,
        })
        .to_string();
        let item = OutboxItem {
            id: "journal:create:journal-plain".to_owned(),
            resource: "journal".to_owned(),
            operation: "create".to_owned(),
            object_id: "journal-plain".to_owned(),
            payload_json,
            attempt_count: 0,
        };

        let api = FakeApiClient::new()
            .with_json_fixture(
                "POST_JSON /v3/sync/journals",
                json!({ "id": "journal-plain", "name": name }),
            )
            .await;

        push_journal_create_outbox(&store, &api, profile.id, &item)
            .await
            .expect("create push should succeed");

        // Wire carries the name verbatim.
        let bodies = api.captured_post_json_bodies("/v3/sync/journals").await;
        let sent = bodies.first().expect("create should POST one body");
        assert_eq!(sent.get("name").and_then(Value::as_str), Some(name));

        // Local row is verbatim too.
        let row = store
            .get_json_row_by_id("journals", "journal-plain")
            .expect("journal row read should succeed")
            .expect("journal row should exist locally");
        let local: Value = serde_json::from_str(&row).expect("local row is json");
        assert_eq!(local.get("name").and_then(Value::as_str), Some(name));
    }
}

#[cfg(test)]
mod entry_put_outcome_tests {
    use super::*;

    #[test]
    fn entry_put_outcomes_fail_closed() {
        assert!(validate_entry_put_outcome(&json!({}), "update").is_ok());
        assert!(validate_entry_put_outcome(&json!({ "outcome": "clean" }), "update").is_ok());

        let dirty = validate_entry_put_outcome(&json!({ "outcome": "dirty" }), "update")
            .expect_err("dirty outcome should fail");
        assert!(dirty.to_string().contains("outcome=dirty"));

        let unknown = validate_entry_put_outcome(&json!({ "outcome": "surprise" }), "update")
            .expect_err("unknown outcome should fail closed");
        assert!(unknown.to_string().contains("unrecognized outcome"));
        assert!(!unknown.to_string().contains("surprise"));

        let null = validate_entry_put_outcome(&json!({ "outcome": null }), "update")
            .expect_err("null outcome should fail closed");
        assert!(null.to_string().contains("unrecognized outcome"));
    }
}
