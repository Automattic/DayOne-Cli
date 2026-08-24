use super::crypto::{
    JournalFieldDecryptOutcome, build_entries_decryptor, build_journal_decryptor,
    inspect_journal_metadata, journal_vault_key, normalize_known_encrypted_journal_name,
    try_decrypt_journal_fields_with_key,
};
use super::entries::run_entries_feed_resource;
use super::resources::{
    RequiredSingletonError, persist_items_from_array, run_cursor_resource,
    run_required_singleton_resource, run_singleton_resource,
};
use super::*;

pub(crate) async fn fetch_pre_sync_resources<C: DayOneApiClient>(
    api: &C,
    store: &Store,
    run_id: i64,
    resources: &mut Vec<ResourceSyncOutput>,
) -> std::result::Result<FeatureFlags, RequiredSingletonError> {
    // Keep profile fresh before pull, but do not fail the run if this
    // non-gating endpoint is temporarily unavailable.
    run_singleton_resource(
        store,
        run_id,
        resources,
        "user_profile",
        || async { api.get_json("/users/", &[]).await },
        |store, payload| {
            store.upsert_singleton_json_row("user_profile", 1, &payload.to_string(), None)?;
            Ok(1)
        },
    )
    .await;

    // Feature flags are a required pre-sync input for this helper, so
    // fetch/persist errors are propagated to the caller rather than being
    // ignored locally. Whether the overall sync aborts or falls back is
    // determined by the caller's error-handling policy.
    let feature_flags_payload = run_required_singleton_resource(
        store,
        run_id,
        resources,
        "feature_flags",
        || async { api.get_json("/v2/feature-flags", &[]).await },
        |store, payload| {
            store.upsert_singleton_json_row("feature_flags", 1, &payload.to_string(), None)?;
            Ok(1)
        },
    )
    .await?;

    Ok(FeatureFlags::from_payload(&feature_flags_payload))
}

fn valid_journal_deletion_marker(value: &Value) -> bool {
    match value {
        Value::Number(value) => value.as_f64().is_some_and(|value| value > 0.0),
        Value::String(value) => {
            let value = value.trim();
            value
                .parse::<f64>()
                .is_ok_and(|value| value.is_finite() && value > 0.0)
                || time::OffsetDateTime::parse(
                    value,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok()
        }
        _ => false,
    }
}

fn persist_pulled_journal(
    store: &Store,
    journal_decryptor: Option<&JournalDecryptor>,
    id: &str,
    item: &Value,
) -> Result<()> {
    if let Some(item_id) = item.get("id") {
        let Some(item_id) = item_id.as_str() else {
            bail!("journal payload id is not a string");
        };
        if item_id != id {
            bail!("journal feed id does not match journal payload id");
        }
    }
    let deletion_markers: Vec<&Value> = ["deleted_at", "deletedAt"]
        .into_iter()
        .filter_map(|field| item.get(field))
        .filter(|value| !value.is_null())
        .collect();
    let state_deleted = item
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| state == "deleted");
    if state_deleted
        || deletion_markers
            .iter()
            .any(|value| valid_journal_deletion_marker(value))
    {
        store.delete_local_journal(id)?;
        return Ok(());
    }
    if !deletion_markers.is_empty() {
        bail!("journal deletion timestamp is invalid");
    }

    let mut normalized = item.clone();
    let vault_key = journal_decryptor.and_then(|decryptor| journal_vault_key(item, decryptor));
    let diagnostics: Vec<(String, String)> = inspect_journal_metadata(item, vault_key.as_deref())
        .into_iter()
        .map(|(field, status)| (field.to_owned(), status.to_owned()))
        .collect();
    if journal_decryptor.is_some() {
        let started = std::time::Instant::now();
        match try_decrypt_journal_fields_with_key(&mut normalized, vault_key.as_deref()) {
            Ok(outcome) => crate::diagnostics::record_crypto(
                "journal_fields_decrypt",
                "journal",
                match outcome {
                    JournalFieldDecryptOutcome::Decrypted => "success",
                    JournalFieldDecryptOutcome::Skipped => "skipped",
                    JournalFieldDecryptOutcome::Failed => "error",
                },
                started.elapsed(),
                None,
            ),
            Err(err) => {
                crate::diagnostics::record_crypto(
                    "journal_fields_decrypt",
                    "journal",
                    "error",
                    started.elapsed(),
                    Some(err.as_ref()),
                );
                log_sync(format!("journal decrypt failed id={id}: {err}"));
            }
        }
    }
    if diagnostics
        .iter()
        .any(|(_, status)| status == "unverified_d1")
        && let Some(existing) = store.get_json_row_by_id("journals", id)?
        && let Ok(existing) = serde_json::from_str::<Value>(&existing)
    {
        for (field, status) in &diagnostics {
            if status == "unverified_d1"
                && let Some(value) = existing.get(field)
                && let Some(object) = normalized.as_object_mut()
            {
                object.insert(field.clone(), value.clone());
            }
        }
    }
    normalize_known_encrypted_journal_name(&mut normalized);
    let journal: Journal = serde_json::from_value(normalized.clone())?;

    let observed_fields: Vec<&str> = ["name", "description_v2"]
        .into_iter()
        .filter(|field| item.get(field).is_some())
        .collect();
    let diagnostic_refs: Vec<JournalMetadataDiagnostic<'_>> = diagnostics
        .iter()
        .map(|(field, status)| (field.as_str(), status.as_str()))
        .collect();
    let data_json = serde_json::to_string(&journal)?;
    if !store.apply_pulled_journal(
        id,
        journal.updated_at.as_deref(),
        journal.deleted_at.as_deref(),
        &data_json,
        &observed_fields,
        &diagnostic_refs,
    )? {
        log_sync(format!(
            "journal pull skipped id={id}: local update is pending in outbox"
        ));
    }
    Ok(())
}

fn request_verifiable_metadata_reinspection(
    store: &Store,
    journal_decryptor: &JournalDecryptor,
) -> Result<()> {
    for journal_id in store.journals_with_unverified_metadata()? {
        let Some(raw) = store.get_json_row_by_id("journals", &journal_id)? else {
            continue;
        };
        let Ok(journal) = serde_json::from_str::<Value>(&raw) else {
            log_sync("journal metadata reinspection skipped: stored journal JSON is invalid");
            continue;
        };
        if journal_vault_key(&journal, journal_decryptor).is_none() {
            continue;
        }

        store.request_journal_metadata_inspection()?;
        if store.start_journal_metadata_inspection()? {
            log_sync("requested full journal metadata reinspection after key recovery");
        }
        break;
    }
    Ok(())
}

fn finish_journal_metadata_inspection(store: &Store, journal_pull_succeeded: bool) -> Result<()> {
    if journal_pull_succeeded && store.complete_journal_metadata_inspection()? {
        log_sync("completed full journal metadata inspection");
    }
    Ok(())
}

pub(crate) async fn run_pull_phase<C: DayOneApiClient>(
    api: &C,
    store: &Store,
    run_id: i64,
    resources: &mut Vec<ResourceSyncOutput>,
    feature_flags: &FeatureFlags,
    profile_id: i64,
    user_id: Option<&str>,
) -> Result<()> {
    run_singleton_resource(
        store,
        run_id,
        resources,
        "user_key",
        || async { api.get_json("/users/key", &[]).await },
        |store, payload| {
            store.upsert_singleton_json_row("user_identity_key", 1, &payload.to_string(), None)?;
            Ok(1)
        },
    )
    .await;

    run_cursor_resource(
        api,
        store,
        run_id,
        resources,
        CursorResourceParams {
            resource_key: "content_keys",
            api_path: "/v4/sync/changes/contentKeys",
            static_params: &[],
        },
        |item| {
            item.get("fingerprint")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        },
        |store, id, item| {
            store.upsert_json_row("content_keys", id, None, None, &item.to_string())?;
            Ok(())
        },
    )
    .await;

    run_singleton_resource(
        store,
        run_id,
        resources,
        "named_crypto_keys",
        || async { api.get_json("/v4/sync/named/cryptoKeys", &[]).await },
        |store, payload| {
            store.upsert_singleton_json_row("user_keys", 1, &payload.to_string(), None)?;
            Ok(1)
        },
    )
    .await;

    // Diagnostic metadata must never add a failure path to sync.
    let key_source = if crate::diagnostics::is_tracing() {
        match store.encryption_key_storage_for_profile(profile_id) {
            Ok(Some(storage)) => storage.as_str(),
            Ok(None) => "missing",
            Err(_) => "unknown",
        }
    } else {
        "unknown"
    };
    let key_started = std::time::Instant::now();
    let master_key = match store.get_encryption_key_for_profile(profile_id) {
        Ok(key) => {
            crate::diagnostics::record_crypto(
                "master_key_load",
                key_source,
                if key.is_some() { "success" } else { "skipped" },
                key_started.elapsed(),
                None,
            );
            key
        }
        Err(error) => {
            crate::diagnostics::record_crypto(
                "master_key_load",
                key_source,
                "error",
                key_started.elapsed(),
                Some(&error),
            );
            return Err(error.into());
        }
    };
    let journal_decryptor = match master_key {
        Some(master_key) => {
            let started = std::time::Instant::now();
            match build_journal_decryptor(store, &master_key, user_id) {
                Ok(value) => {
                    crate::diagnostics::record_crypto(
                        "journal_decryptor_init",
                        key_source,
                        if value.is_some() {
                            "success"
                        } else {
                            "skipped"
                        },
                        started.elapsed(),
                        None,
                    );
                    value
                }
                Err(err) => {
                    crate::diagnostics::record_crypto(
                        "journal_decryptor_init",
                        key_source,
                        "error",
                        started.elapsed(),
                        Some(err.as_ref()),
                    );
                    log_sync(format!("journal decryptor init failed: {err}"));
                    None
                }
            }
        }
        None => None,
    };

    if let Some(decryptor) = journal_decryptor.as_ref() {
        request_verifiable_metadata_reinspection(store, decryptor)?;
    }

    let journal_params = build_journals_sync_params(
        store.get_sync_cursor("journals")?,
        store.journal_row_count()?,
    );
    let journal_pull_succeeded = run_cursor_resource(
        api,
        store,
        run_id,
        resources,
        CursorResourceParams {
            resource_key: "journals",
            api_path: "/v6/sync/journals",
            static_params: &journal_params,
        },
        value_id,
        |store, id, item| persist_pulled_journal(store, journal_decryptor.as_ref(), id, item),
    )
    .await;
    finish_journal_metadata_inspection(store, journal_pull_succeeded)?;

    let entries_decryptor = match journal_decryptor.as_ref() {
        Some(decryptor) => {
            let started = std::time::Instant::now();
            match build_entries_decryptor(store, decryptor) {
                Ok(value) => {
                    crate::diagnostics::record_crypto(
                        "entries_decryptor_init",
                        "journal",
                        "success",
                        started.elapsed(),
                        None,
                    );
                    Some(value)
                }
                Err(err) => {
                    crate::diagnostics::record_crypto(
                        "entries_decryptor_init",
                        "journal",
                        "error",
                        started.elapsed(),
                        Some(err.as_ref()),
                    );
                    log_sync(format!("entries decryptor init failed: {err}"));
                    None
                }
            }
        }
        None => None,
    };

    let journal_ids = store.get_sync_enabled_journal_ids()?;
    let mut embeddings_enabled = crate::entry_embeddings::embeddings_enabled();
    for journal_id in journal_ids {
        let resource_key = format!("entries:{journal_id}");
        let entry_path = format!("/v2/sync/entries/{journal_id}/feed");
        let initial_pull = store
            .get_sync_cursor(&resource_key)?
            .unwrap_or_default()
            .is_empty();
        let mut params = vec![("groups".to_owned(), journal_id.clone())];
        if initial_pull {
            params.push(("excludeDeleted".to_owned(), "true".to_owned()));
        }

        store.set_journal_sync_status(&journal_id, JournalSyncStatus::Syncing, false)?;
        run_entries_feed_resource(
            api,
            store,
            run_id,
            resources,
            EntriesFeedResourceParams {
                resource_key: &resource_key,
                api_path: &entry_path,
                static_params: &params,
                entries_decryptor: entries_decryptor.as_ref(),
                embeddings_enabled: &mut embeddings_enabled,
            },
        )
        .await;
        store.set_journal_sync_status(&journal_id, JournalSyncStatus::Idle, true)?;
    }

    run_cursor_resource(
        api,
        store,
        run_id,
        resources,
        CursorResourceParams {
            resource_key: "notifications",
            api_path: "/users/notifications/feed",
            static_params: &[],
        },
        value_id,
        |store, id, item| {
            let updated_at = item.get("updated_at").and_then(Value::as_str);
            let deleted_at = item.get("deleted_at").and_then(Value::as_str);
            store.upsert_json_row(
                "notifications",
                id,
                updated_at,
                deleted_at,
                &item.to_string(),
            )?;
            Ok(())
        },
    )
    .await;

    run_singleton_resource(
        store,
        run_id,
        resources,
        "unread_entries",
        || async { api.get_json("/shares/unread-entries", &[]).await },
        |store, payload| persist_items_from_array(store, "unread_entries", payload, None),
    )
    .await;

    run_cursor_resource(
        api,
        store,
        run_id,
        resources,
        CursorResourceParams {
            resource_key: "templates",
            api_path: "/v4/sync/changes/template",
            static_params: &[],
        },
        value_id,
        |store, id, item| {
            let updated_at = item.get("updated_at").and_then(Value::as_str);
            let deleted_at = item.get("deleted_at").and_then(Value::as_str);
            store.upsert_json_row("templates", id, updated_at, deleted_at, &item.to_string())?;
            Ok(())
        },
    )
    .await;

    run_singleton_resource(
        store,
        run_id,
        resources,
        "user_settings",
        || async {
            let cursor = store.get_sync_cursor("user_settings")?.unwrap_or_default();
            let mut query = Vec::new();
            if !cursor.is_empty() {
                query.push(("cursor", cursor));
            }
            Ok(api
                .get_json_allow_304("/user-settings", &query)
                .await?
                .unwrap_or(Value::Null))
        },
        |store, payload| {
            if payload.is_null() {
                return Ok(0);
            }
            let cursor = payload.get("cursor").and_then(Value::as_str);
            store.upsert_user_settings(cursor, &payload.to_string())?;
            if let Some(c) = cursor {
                store.set_sync_cursor("user_settings", Some(c))?;
            }
            Ok(1)
        },
    )
    .await;

    run_cursor_resource(
        api,
        store,
        run_id,
        resources,
        CursorResourceParams {
            resource_key: "pbc_template_categories",
            api_path: "/v2/templates/categories",
            static_params: &[],
        },
        value_id,
        |store, id, item| {
            store.upsert_json_row("pbc_template_categories", id, None, None, &item.to_string())?;
            Ok(())
        },
    )
    .await;

    run_cursor_resource(
        api,
        store,
        run_id,
        resources,
        CursorResourceParams {
            resource_key: "pbc_templates",
            api_path: "/v2/templates",
            static_params: &[],
        },
        value_id,
        |store, id, item| {
            store.upsert_json_row("pbc_templates", id, None, None, &item.to_string())?;
            Ok(())
        },
    )
    .await;

    run_cursor_resource(
        api,
        store,
        run_id,
        resources,
        CursorResourceParams {
            resource_key: "pbc_prompt_pack_gallery",
            api_path: "/pbcms/gallery",
            static_params: &[],
        },
        value_id,
        |store, id, item| {
            store.upsert_json_row("pbc_prompt_pack_gallery", id, None, None, &item.to_string())?;
            Ok(())
        },
    )
    .await;

    let user_identity_key = store.get_user_identity_key_json()?;
    let content_key_bundle = store.get_json_row_by_id("user_keys", "1")?;
    let master_key = store.get_encryption_key_for_profile(profile_id)?;
    let crypto_ctx = build_crypto_context_from_sources(
        master_key,
        user_identity_key.as_deref(),
        content_key_bundle.as_deref(),
    );

    if feature_flags.journal_hierarchy {
        run_cursor_resource(
            api,
            store,
            run_id,
            resources,
            CursorResourceParams {
                resource_key: "journal_hierarchy",
                api_path: "/v1/journals/hierarchy",
                static_params: &[],
            },
            value_id,
            |store, id, item| {
                let updated_at = item.get("updated_at").and_then(Value::as_str);
                let deleted_at = item.get("deleted_at").and_then(Value::as_str);
                store.upsert_json_row(
                    "journal_hierarchy_items",
                    id,
                    updated_at,
                    deleted_at,
                    &item.to_string(),
                )?;
                Ok(())
            },
        )
        .await;
    }

    if feature_flags.daily_chat {
        run_cursor_resource(
            api,
            store,
            run_id,
            resources,
            CursorResourceParams {
                resource_key: "daily_chat_feed",
                api_path: "/labs/ai/daily-chat/feed",
                static_params: &[],
            },
            value_id,
            |store, id, item| {
                let decrypted = decrypt_payload(item, &crypto_ctx)?;
                if let Some(existing) = store.get_json_row_by_id("daily_chat_feed", id)?
                    && let Ok(local) = serde_json::from_str::<Value>(&existing)
                    && local_is_newer(&local, &decrypted)
                {
                    store.enqueue_outbox_item_if_not_pending(
                        "daily_chat_feed",
                        "update",
                        id,
                        &local.to_string(),
                    )?;
                }
                let updated_at = decrypted.get("updated_at").and_then(Value::as_str);
                let deleted_at = decrypted.get("deleted_at").and_then(Value::as_str);
                store.upsert_json_row(
                    "daily_chat_feed",
                    id,
                    updated_at,
                    deleted_at,
                    &decrypted.to_string(),
                )?;
                Ok(())
            },
        )
        .await;

        run_singleton_resource(
            store,
            run_id,
            resources,
            "daily_chat_settings",
            || async { api.get_json("/labs/ai/daily-chat/settings", &[]).await },
            |store, payload| {
                let decrypted = decrypt_payload(payload, &crypto_ctx)?;
                if let Some(existing) = store.get_json_row_by_id("daily_chat_settings", "1")?
                    && let Ok(local) = serde_json::from_str::<Value>(&existing)
                    && local_is_newer(&local, &decrypted)
                {
                    store.enqueue_outbox_item_if_not_pending(
                        "daily_chat_settings",
                        "update",
                        "settings",
                        &local.to_string(),
                    )?;
                }
                store.upsert_singleton_json_row(
                    "daily_chat_settings",
                    1,
                    &decrypted.to_string(),
                    decrypted.get("updated_at").and_then(Value::as_str),
                )?;
                Ok(1)
            },
        )
        .await;
    }

    if feature_flags.context_library {
        run_cursor_resource(
            api,
            store,
            run_id,
            resources,
            CursorResourceParams {
                resource_key: "context_library",
                api_path: "/labs/context-library/feed",
                static_params: &[("limit".to_owned(), "100".to_owned())],
            },
            value_id,
            |store, id, item| {
                let decrypted = decrypt_payload(item, &crypto_ctx)?;
                let existing = store.get_json_row_by_id("context_library_items", id)?;
                let needs_sync = store
                    .get_json_row_needs_sync("context_library_items", id)?
                    .unwrap_or(false);
                if let Some(existing_json) = existing.as_deref()
                    && let Ok(local) = serde_json::from_str::<Value>(existing_json)
                {
                    if needs_sync {
                        store.enqueue_outbox_item_if_not_pending(
                            "context_item",
                            "update",
                            id,
                            &local.to_string(),
                        )?;
                        return Ok(());
                    }
                    if local_is_newer(&local, &decrypted) {
                        store.enqueue_outbox_item_if_not_pending(
                            "context_item",
                            "update",
                            id,
                            &local.to_string(),
                        )?;
                        return Ok(());
                    }
                }
                let updated_at = decrypted.get("updated_at").and_then(Value::as_str);
                let deleted_at = decrypted.get("deleted_at").and_then(Value::as_str);
                store.upsert_json_row_with_sync_state(
                    "context_library_items",
                    id,
                    updated_at,
                    deleted_at,
                    Some(false),
                    &decrypted.to_string(),
                )?;
                Ok(())
            },
        )
        .await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::rand_core::OsRng;
    use rusqlite::OptionalExtension;
    use serde_json::json;
    use tempfile::Builder;

    struct TestStorePath {
        path: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    impl std::ops::Deref for TestStorePath {
        type Target = std::path::Path;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    fn test_store_path(name: &str) -> TestStorePath {
        let dir = Builder::new()
            .prefix(&format!("dayone-cli-sync-journals-tests-{name}-"))
            .tempdir()
            .expect("temp dir should create");
        let path = dir.path().join("dayone.db");
        TestStorePath { path, _dir: dir }
    }

    fn diagnostic_status(store: &Store, journal_id: &str, field: &str) -> Option<String> {
        store
            .connect()
            .expect("connection should open")
            .query_row(
                "SELECT status FROM journal_metadata_diagnostics WHERE journal_id = ?1 AND field = ?2",
                rusqlite::params![journal_id, field],
                |row| row.get(0),
            )
            .optional()
            .expect("diagnostic query should work")
    }

    #[test]
    fn journal_pull_rejects_mismatched_tombstone_before_changing_local_state() {
        let path = test_store_path("metadata-diagnostic-id-mismatch");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_json_row(
                "journals",
                "trusted-id",
                None,
                None,
                &json!({ "id": "trusted-id", "name": "Local" }).to_string(),
            )
            .expect("trusted journal should save");
        store
            .replace_journal_metadata_diagnostics("trusted-id", &[("name", "plaintext")])
            .expect("trusted diagnostic should save");

        let error = persist_pulled_journal(
            &store,
            None,
            "trusted-id",
            &json!({
                "id": "payload-id",
                "state": "deleted"
            }),
        )
        .expect_err("mismatched ids should fail");

        assert!(error.to_string().contains("does not match"));
        assert!(
            store
                .get_json_row_by_id("journals", "trusted-id")
                .expect("journal read should work")
                .is_some()
        );
        assert_eq!(
            diagnostic_status(&store, "trusted-id", "name").as_deref(),
            Some("plaintext")
        );
        assert_eq!(diagnostic_status(&store, "payload-id", "name"), None);

        let error = persist_pulled_journal(
            &store,
            None,
            "trusted-id",
            &json!({ "id": 123, "state": "deleted" }),
        )
        .expect_err("non-string payload ids should fail");
        assert!(error.to_string().contains("not a string"));
        assert!(
            store
                .get_json_row_by_id("journals", "trusted-id")
                .expect("trusted journal should load")
                .is_some()
        );
    }

    #[test]
    fn journal_pull_persists_non_string_metadata_diagnostic() {
        let path = test_store_path("non-string-metadata");
        let store = Store::open_at(&*path).expect("store should open");

        persist_pulled_journal(
            &store,
            None,
            "journal-malformed",
            &json!({
                "id": "journal-malformed",
                "name": 123,
                "encryption": { "vault": {} }
            }),
        )
        .expect("non-string metadata should not reject the journal");

        assert_eq!(
            diagnostic_status(&store, "journal-malformed", "name").as_deref(),
            Some("malformed_d1")
        );
    }

    #[test]
    fn journal_metadata_inspection_state_clears_only_after_successful_pull() {
        let path = test_store_path("metadata-inspection-completion");
        let store = Store::open_at(&*path).expect("store should open");

        finish_journal_metadata_inspection(&store, true)
            .expect("an inspection that never started should remain requested");
        assert!(
            store
                .journal_metadata_inspection_is_pending()
                .expect("inspection state should load")
        );

        assert!(
            store
                .start_journal_metadata_inspection()
                .expect("inspection should start")
        );
        finish_journal_metadata_inspection(&store, false)
            .expect("failed inspection should retain running state");
        assert!(
            store
                .journal_metadata_inspection_is_pending()
                .expect("inspection state should load")
        );

        finish_journal_metadata_inspection(&store, true)
            .expect("successful inspection should clear running state");
        assert!(
            !store
                .journal_metadata_inspection_is_pending()
                .expect("inspection state should load")
        );
    }

    #[test]
    fn journal_pull_replaces_and_clears_privacy_safe_metadata_diagnostics() {
        let path = test_store_path("metadata-diagnostics");
        let store = Store::open_at(&*path).expect("store should open");
        let mut rng = OsRng;
        let user_private =
            RsaPrivateKey::new(&mut rng, 2048).expect("user private key should generate");
        let vault_key = [7_u8; 32];
        let encrypted_vault_key = user_private
            .to_public_key()
            .encrypt(&mut rng, Oaep::new::<Sha1>(), &vault_key)
            .expect("vault key should wrap");
        let decryptor = JournalDecryptor {
            user_id: Some("user-1".to_owned()),
            user_private_key: user_private,
            user_verify_keys: std::collections::HashMap::new(),
        };
        let plaintext = json!({
            "id": "journal-e2e",
            "name": "Private Journal",
            "updated_at": "2026-07-06T00:00:00.000Z",
            "connected_services": ["strava"],
            "future_server_field": { "nested": true },
            "encryption": { "vault": { "grants": [{
                "encrypted_vault_key": BASE64_STANDARD.encode(encrypted_vault_key)
            }] } }
        });

        persist_pulled_journal(&store, None, "journal-e2e", &plaintext)
            .expect("plaintext server metadata should persist");
        assert_eq!(
            diagnostic_status(&store, "journal-e2e", "name").as_deref(),
            Some("plaintext")
        );

        let mut encrypted = plaintext.clone();
        encrypted["name"] = json!(
            crate::sync::crypto::encrypt_d1_mode0_with_key(b"Private Journal", &vault_key)
                .expect("name should encrypt")
        );
        encrypted
            .as_object_mut()
            .expect("journal should be an object")
            .remove("updated_at");
        persist_pulled_journal(&store, None, "journal-e2e", &encrypted)
            .expect("revisionless unverified metadata should persist");
        assert_eq!(
            diagnostic_status(&store, "journal-e2e", "name").as_deref(),
            Some("unverified_d1")
        );
        assert_eq!(
            store
                .get_json_row_by_id("journals", "journal-e2e")
                .expect("journal should load")
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|journal| journal["name"].as_str().map(ToOwned::to_owned))
                .as_deref(),
            Some("Private Journal"),
            "an unverified full pull must preserve readable local metadata"
        );
        assert_eq!(
            store
                .journal_metadata_diagnostics("journal-e2e")
                .expect("diagnostics should load"),
            vec![("name".to_owned(), "unverified_d1".to_owned())],
            "diagnostics must not retain raw server metadata"
        );

        store
            .set_sync_cursor("journals", Some("historical-cursor"))
            .expect("journal cursor should save");
        request_verifiable_metadata_reinspection(&store, &decryptor)
            .expect("key recovery should request a full journal pull");
        assert_eq!(
            store
                .get_sync_cursor("journals")
                .expect("journal cursor should load"),
            None
        );
        persist_pulled_journal(&store, Some(&decryptor), "journal-e2e", &encrypted)
            .expect("full pull should verify revisionless metadata");
        finish_journal_metadata_inspection(&store, true)
            .expect("successful reinspection should complete");
        assert_eq!(diagnostic_status(&store, "journal-e2e", "name"), None);
        let row = store
            .get_json_row_by_id("journals", "journal-e2e")
            .expect("journal read should work")
            .expect("journal should exist");
        let journal: Value = serde_json::from_str(&row).expect("journal should be json");
        assert_eq!(journal["name"], "Private Journal");
        assert_eq!(journal["future_server_field"]["nested"], true);

        let mut pending_local = plaintext.clone();
        pending_local["id"] = json!("journal-pending-valid");
        pending_local["name"] = json!("Local pending edit");
        pending_local["description_v2"] = json!("Local description");
        store
            .upsert_json_row(
                "journals",
                "journal-pending-valid",
                None,
                None,
                &pending_local.to_string(),
            )
            .expect("pending local journal should save");
        store
            .enqueue_outbox_item(
                "journal:update:journal-pending-valid",
                "journal",
                "update",
                "journal-pending-valid",
                "{}",
                0,
            )
            .expect("pending update should enqueue");
        store
            .replace_journal_metadata_diagnostics(
                "journal-pending-valid",
                &[("name", "plaintext"), ("description_v2", "plaintext")],
            )
            .expect("stale diagnostics should save");
        let mut pending_wire = encrypted.clone();
        pending_wire["id"] = json!("journal-pending-valid");
        persist_pulled_journal(
            &store,
            Some(&decryptor),
            "journal-pending-valid",
            &pending_wire,
        )
        .expect("verified server metadata should be inspected");
        assert_eq!(
            store
                .get_json_row_by_id("journals", "journal-pending-valid")
                .expect("pending journal should load")
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|journal| journal["name"].as_str().map(ToOwned::to_owned))
                .as_deref(),
            Some("Local pending edit"),
            "verified metadata must not replace the pending local row"
        );
        assert_eq!(
            store
                .journal_metadata_diagnostics("journal-pending-valid")
                .expect("diagnostics should load"),
            vec![("description_v2".to_owned(), "plaintext".to_owned())],
            "verified observed fields clear while omitted fields remain"
        );
        let mut explicit_clear = pending_wire.clone();
        explicit_clear
            .as_object_mut()
            .expect("journal should be an object")
            .remove("name");
        explicit_clear["description_v2"] = json!("");
        persist_pulled_journal(
            &store,
            Some(&decryptor),
            "journal-pending-valid",
            &explicit_clear,
        )
        .expect("explicitly cleared metadata should be inspected");
        assert!(
            store
                .journal_metadata_diagnostics("journal-pending-valid")
                .expect("diagnostics should load")
                .is_empty(),
            "explicit empty fields must clear stale diagnostics"
        );

        let mut wrong_key_wire = plaintext.clone();
        wrong_key_wire["id"] = json!("journal-wrong-key");
        wrong_key_wire["name"] = json!(
            crate::sync::crypto::encrypt_d1_mode0_with_key(b"Private Journal", &[8_u8; 32])
                .expect("name should encrypt with the wrong key")
        );
        wrong_key_wire
            .as_object_mut()
            .expect("journal should be an object")
            .remove("updated_at");
        persist_pulled_journal(&store, None, "journal-wrong-key", &wrong_key_wire)
            .expect("unverified wrong-key metadata should persist");
        request_verifiable_metadata_reinspection(&store, &decryptor)
            .expect("available vault key should request reinspection");
        persist_pulled_journal(
            &store,
            Some(&decryptor),
            "journal-wrong-key",
            &wrong_key_wire,
        )
        .expect("wrong-key metadata should be classified");
        finish_journal_metadata_inspection(&store, true)
            .expect("wrong-key inspection should complete");
        assert_eq!(
            diagnostic_status(&store, "journal-wrong-key", "name").as_deref(),
            Some("undecryptable_d1")
        );

        store
            .upsert_json_row(
                "journals",
                "journal-key-unavailable",
                None,
                None,
                &json!({
                    "id": "journal-key-unavailable",
                    "name": "Encrypted Journal",
                    "encryption": { "vault": { "grants": [{
                        "encrypted_vault_key": "not-base64"
                    }] } }
                })
                .to_string(),
            )
            .expect("unavailable-key journal should save");
        store
            .replace_journal_metadata_diagnostics(
                "journal-key-unavailable",
                &[("name", "unverified_d1")],
            )
            .expect("unverified diagnostic should save");
        store
            .set_sync_cursor("journals", Some("historical-cursor"))
            .expect("historical cursor should save");
        request_verifiable_metadata_reinspection(&store, &decryptor)
            .expect("unavailable keys should be checked");
        assert_eq!(
            store
                .get_sync_cursor("journals")
                .expect("journal cursor should load")
                .as_deref(),
            Some("historical-cursor"),
            "unavailable keys must not trigger repeated full pulls"
        );
        assert!(
            !store
                .journal_metadata_inspection_is_pending()
                .expect("inspection state should load")
        );
    }

    #[test]
    fn journal_pull_preserves_local_row_when_update_is_inflight() {
        let path = test_store_path("pending-update-preserved");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_json_row(
                "journals",
                "journal-1",
                Some("2026-07-06T00:00:00.000Z"),
                None,
                &json!({
                    "id": "journal-1",
                    "name": "local pending update",
                    "description_v2": "local description",
                    "updated_at": "2026-07-06T00:00:00.000Z",
                    "encryption": { "vault": {} }
                })
                .to_string(),
            )
            .expect("local journal should save");
        store
            .enqueue_outbox_item(
                "journal:update:journal-1",
                "journal",
                "update",
                "journal-1",
                "{}",
                0,
            )
            .expect("pending update should enqueue");
        store
            .replace_journal_metadata_diagnostics(
                "journal-1",
                &[("name", "unverified_d1"), ("description_v2", "plaintext")],
            )
            .expect("existing diagnostic should save");

        persist_pulled_journal(
            &store,
            None,
            "journal-1",
            &json!({
                "id": "journal-1",
                "name": "older server echo",
                "updated_at": "2026-07-05T00:00:00.000Z",
                "encryption": { "vault": {} }
            }),
        )
        .expect("pull persist should succeed");

        let row = store
            .get_json_row_by_id("journals", "journal-1")
            .expect("journal read should work")
            .expect("journal row should still exist");
        let local: Value = serde_json::from_str(&row).expect("journal row should be json");
        assert_eq!(
            local.get("name").and_then(Value::as_str),
            Some("local pending update")
        );
        assert_eq!(
            local.get("description_v2").and_then(Value::as_str),
            Some("local description")
        );
        assert_eq!(
            diagnostic_status(&store, "journal-1", "name").as_deref(),
            Some("plaintext"),
            "the raw server observation should be recorded without replacing the local row"
        );
        assert_eq!(
            diagnostic_status(&store, "journal-1", "description_v2").as_deref(),
            Some("plaintext"),
            "a skipped row must not clear an existing field that the response cannot order"
        );
    }

    #[test]
    fn journal_pull_applies_numeric_camel_case_tombstone() {
        let path = test_store_path("numeric-journal-tombstone");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_json_row(
                "journals",
                "journal-1",
                None,
                None,
                &json!({ "id": "journal-1", "name": "local" }).to_string(),
            )
            .expect("local journal should save");
        store
            .replace_journal_metadata_diagnostics("journal-1", &[("name", "plaintext")])
            .expect("diagnostic should save");

        persist_pulled_journal(
            &store,
            None,
            "journal-1",
            &json!({
                "id": "journal-1",
                "deleted_at": null,
                "deletedAt": 1_786_000_000
            }),
        )
        .expect("numeric tombstone should apply");

        assert!(
            store
                .get_json_row_by_id("journals", "journal-1")
                .expect("journal read should work")
                .is_none()
        );
        assert_eq!(diagnostic_status(&store, "journal-1", "name"), None);
    }

    #[test]
    fn journal_pull_rejects_invalid_deletion_markers_without_deleting_local_data() {
        let path = test_store_path("invalid-journal-tombstones");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_json_row(
                "journals",
                "journal-1",
                None,
                None,
                &json!({ "id": "journal-1", "name": "local" }).to_string(),
            )
            .expect("local journal should save");

        for marker in [
            json!(0),
            json!(-1),
            json!(""),
            json!("0"),
            json!("not-a-date"),
        ] {
            persist_pulled_journal(
                &store,
                None,
                "journal-1",
                &json!({ "id": "journal-1", "deletedAt": marker }),
            )
            .expect_err("invalid deletion marker should fail");
            assert!(
                store
                    .get_json_row_by_id("journals", "journal-1")
                    .expect("journal read should work")
                    .is_some(),
                "invalid deletion marker must not delete local data"
            );
        }
    }

    #[test]
    fn journal_pull_overwrites_when_no_update_is_inflight() {
        let path = test_store_path("no-pending-update-overwrites");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_json_row(
                "journals",
                "journal-1",
                Some("2026-07-05T00:00:00.000Z"),
                None,
                &json!({
                    "id": "journal-1",
                    "name": "local old",
                    "updated_at": "2026-07-05T00:00:00.000Z"
                })
                .to_string(),
            )
            .expect("local journal should save");

        persist_pulled_journal(
            &store,
            None,
            "journal-1",
            &json!({
                "id": "journal-1",
                "name": "server latest",
                "description_v2": "server description",
                "updated_at": "2026-07-06T00:00:00.000Z"
            }),
        )
        .expect("pull persist should succeed");

        let row = store
            .get_json_row_by_id("journals", "journal-1")
            .expect("journal read should work")
            .expect("journal row should still exist");
        let local: Value = serde_json::from_str(&row).expect("journal row should be json");
        assert_eq!(
            local.get("name").and_then(Value::as_str),
            Some("server latest")
        );
        assert_eq!(
            local.get("description_v2").and_then(Value::as_str),
            Some("server description")
        );
    }

    #[test]
    fn journal_pull_tombstone_still_applies_when_update_is_inflight() {
        let path = test_store_path("pending-update-remote-delete");
        let store = Store::open_at(&*path).expect("store should open");
        store
            .upsert_json_row(
                "journals",
                "journal-1",
                Some("2026-07-06T00:00:00.000Z"),
                None,
                &json!({
                    "id": "journal-1",
                    "name": "local pending update",
                    "updated_at": "2026-07-06T00:00:00.000Z"
                })
                .to_string(),
            )
            .expect("local journal should save");
        store
            .enqueue_outbox_item(
                "journal:update:journal-1",
                "journal",
                "update",
                "journal-1",
                "{}",
                0,
            )
            .expect("pending update should enqueue");
        store
            .replace_journal_metadata_diagnostics("journal-1", &[("name", "plaintext")])
            .expect("diagnostic should save");

        persist_pulled_journal(
            &store,
            None,
            "journal-1",
            &json!({
                "id": "journal-1",
                "name": "deleted server journal",
                "state": "deleted",
                "updated_at": "2026-07-07T00:00:00.000Z"
            }),
        )
        .expect("pull persist should succeed");

        assert!(
            store
                .get_json_row_by_id("journals", "journal-1")
                .expect("journal read should work")
                .is_none(),
            "remote tombstone should delete local journal even with pending update"
        );
        assert_eq!(
            diagnostic_status(&store, "journal-1", "name"),
            None,
            "deleting a journal should clear its diagnostic state"
        );
    }
}
