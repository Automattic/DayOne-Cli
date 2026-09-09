use std::time::Instant;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::http::DeviceInfo;
use crate::store::sqlite::Store;
use crate::store::user_id_from_session_json;
use crate::sync::api::SyncApiClient;
use crate::sync::phases::{outbox, pull};

pub(crate) const LOCK_TTL_MS: i64 = 15 * 60 * 1_000;
pub(crate) const OUTBOX_LEASE_TTL_MS: i64 = 15 * 60 * 1_000;
pub(crate) const MAX_PAGE_LOOPS: usize = 10_000;
pub(crate) const OUTBOX_BATCH_SIZE: usize = 50;
pub(crate) const OUTBOX_MAX_ATTEMPTS: i64 = 8;
pub(crate) const OUTBOX_DEFER_DELAY_MS: i64 = 30_000;

#[derive(Debug, Serialize)]
pub struct ResourceSyncOutput {
    pub resource: String,
    pub started_at_epoch_ms: i64,
    pub finished_at_epoch_ms: i64,
    pub changed_count: i64,
    pub cursor_advanced: bool,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SyncOutput {
    pub ok: bool,
    pub locked: bool,
    pub fatal_error: Option<String>,
    #[serde(skip)]
    pub failure: Option<anyhow::Error>,
    pub resources: Vec<ResourceSyncOutput>,
}

fn prepare_sync_cursors(store: &Store, ignore_existing_cursors: bool) -> Result<()> {
    if ignore_existing_cursors {
        store.clear_sync_cursors()?;
        log_sync("cleared stored sync cursors before run");
    }
    if store.start_journal_metadata_inspection()? {
        log_sync("scheduled full journal pull for metadata inspection");
    }
    Ok(())
}

pub async fn run_sync(
    store: &Store,
    base_url: &str,
    ignore_existing_cursors: bool,
) -> Result<SyncOutput> {
    let sync_started = Instant::now();
    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;

    if !store.try_acquire_sync_lock(LOCK_TTL_MS)? {
        log_sync("sync lock already held; skipping run");
        crate::diagnostics::record_sync_run("finished", "locked", None, sync_started.elapsed());
        return Ok(SyncOutput {
            ok: true,
            locked: true,
            fatal_error: None,
            failure: None,
            resources: vec![],
        });
    }

    let run_id = store.create_sync_run()?;
    crate::diagnostics::record_sync_run("started", "success", Some(run_id), sync_started.elapsed());
    log_sync(format!("starting run_id={run_id} base_url={base_url}"));
    let mut resources = Vec::new();

    let result = async {
        prepare_sync_cursors(store, ignore_existing_cursors)?;
        let user_id = user_id_from_session_json(&session.user_json)?;
        let device_info = DeviceInfo::resolve(user_id.as_deref());
        let api = SyncApiClient::new(base_url, &session.token, device_info)?;
        let stale_before_epoch_ms = now_epoch_ms().saturating_sub(OUTBOX_LEASE_TTL_MS);
        store.reset_processing_outbox_items(stale_before_epoch_ms)?;

        let pre_sync_started = Instant::now();
        let feature_flags =
            match pull::fetch_pre_sync_resources(&api, store, run_id, &mut resources).await {
                Ok(flags) => {
                    crate::diagnostics::record_sync_phase(
                        "pre_sync",
                        "success",
                        pre_sync_started.elapsed(),
                    );
                    flags
                }
                Err(err) => {
                    crate::diagnostics::record_sync_phase(
                        "pre_sync",
                        "error",
                        pre_sync_started.elapsed(),
                    );
                    match err.kind() {
                        pull::RequiredSingletonErrorKind::Fetch => {
                            let message = err.to_string();
                            log_sync(format!(
                                "run_id={run_id} pre-sync feature flags fetch failed; falling back to persisted/default flags: {message}"
                            ));
                            store
                                .get_json_row_by_id("feature_flags", "1")?
                                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                                .map(|payload| pull::FeatureFlags::from_payload(&payload))
                                .unwrap_or_default()
                        }
                        pull::RequiredSingletonErrorKind::Persist => {
                            return Err(err.into_inner());
                        }
                    }
                }
            };

        let pull_started = Instant::now();
        let pull_result = pull::run_pull_phase(
            &api,
            store,
            run_id,
            &mut resources,
            &feature_flags,
            session.profile_id,
            user_id.as_deref(),
        )
        .await;
        crate::diagnostics::record_sync_phase(
            "pull",
            if pull_result.is_ok() { "success" } else { "error" },
            pull_started.elapsed(),
        );
        pull_result?;

        let outbox_started = Instant::now();
        let outbox_result = outbox::drain_sync_outbox(
            store,
            &api,
            run_id,
            session.profile_id,
            &mut resources,
            "outbox:post",
        )
        .await;
        crate::diagnostics::record_sync_phase(
            "outbox",
            if outbox_result.is_ok() {
                "success"
            } else {
                "error"
            },
            outbox_started.elapsed(),
        );
        outbox_result?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    store.release_sync_lock()?;
    record_resource_diagnostics(&resources);
    match result {
        Ok(()) => {
            store.finish_sync_run(run_id, true, None)?;
            crate::diagnostics::record_sync_run(
                "finished",
                "success",
                Some(run_id),
                sync_started.elapsed(),
            );
            log_sync(format!("completed run_id={run_id} status=success"));
            Ok(SyncOutput {
                ok: true,
                locked: false,
                fatal_error: None,
                failure: None,
                resources,
            })
        }
        Err(err) => {
            let message = err.to_string();
            store.finish_sync_run(run_id, false, Some(&message))?;
            crate::diagnostics::record_sync_run(
                "finished",
                "error",
                Some(run_id),
                sync_started.elapsed(),
            );
            log_sync(format!(
                "completed run_id={run_id} status=failed error={message}"
            ));
            Ok(SyncOutput {
                ok: false,
                locked: false,
                fatal_error: Some(message),
                failure: Some(err),
                resources,
            })
        }
    }
}

fn record_resource_diagnostics(resources: &[ResourceSyncOutput]) {
    for resource in resources {
        let duration_ms = resource
            .finished_at_epoch_ms
            .saturating_sub(resource.started_at_epoch_ms);
        crate::diagnostics::record_sync_resource(
            &resource.resource,
            &resource.status,
            resource.changed_count,
            resource.cursor_advanced,
            u64::try_from(duration_ms).unwrap_or_default(),
        );
    }
}

pub(crate) fn now_epoch_ms() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

pub(crate) fn log_sync(message: impl AsRef<str>) {
    eprintln!("[sync] {}", message.as_ref());
}

#[cfg(test)]
pub(crate) use crate::sync::phases::outbox::should_defer_outbox_item_until_keys;
#[cfg(test)]
pub(crate) use crate::sync::phases::outbox::{
    EntryOutboxPayload, OriginalMediaOutboxPayload, OutboxProcessError,
    build_entry_content_from_local, build_entry_envelope_moments, build_entry_thumbnail_parts,
    decode_context_item_outbox_payload, decode_daily_chat_feed_outbox_payload,
    decode_entry_outbox_payload, decode_journal_create_outbox_payload,
    decode_original_media_outbox_payload, mark_retry_or_failed, parse_entry_put_response_bytes,
    push_entry_outbox, resolve_outbox_edit_date_epoch_ms, retry_delay_ms,
};
#[cfg(test)]
pub(crate) use crate::sync::phases::pull::{
    FeatureFlags, build_journals_sync_params, extract_items, feature_enabled,
    is_deleted_journal_entries_feed_error, merge_payload_value, parse_entries_feed_bytes,
};

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
