use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::comment_codec::encode_comment_content_for_journal;
use crate::http::{DayOneApiClient, MultipartBinaryPart};
use crate::models::{Entry, Journal, Moment};
use crate::store::entries::json::rebuild_entry_content;
use crate::store::sqlite::{EntryAttachmentRow, Store};
use crate::store::traits::OutboxStore;
use crate::store::{OutboxItem, StoreError, SyncResourceRunSave};
use crate::sync::crypto::{
    CryptoContext, build_crypto_context_from_sources, decrypt_payload_owned,
    encrypt_context_library_payload, encrypt_entry_content_for_journal,
    encrypt_journal_metadata_for_push, encrypt_original_media_for_journal, encrypt_payload,
    is_e2e_key_material_unavailable_error, is_missing_active_content_public_key_error,
    journal_is_e2e_encrypted,
};
use crate::sync::engine::{
    OUTBOX_BATCH_SIZE, OUTBOX_DEFER_MISSING_KEYS_DELAY_MS, OUTBOX_MAX_ATTEMPTS, ResourceSyncOutput,
    log_sync, now_epoch_ms,
};
use crate::sync::phases::pull::crypto::{build_journal_decryptor, try_decrypt_journal_fields};
use crate::util::encode_path_segment;

#[derive(Debug, thiserror::Error)]
pub(crate) enum OutboxProcessError {
    #[error("{reason}")]
    NonRetryable { reason: String },
    #[error("{0}")]
    Retryable(#[from] anyhow::Error),
}

pub(crate) type OutboxProcessResult<T> = std::result::Result<T, OutboxProcessError>;

impl From<StoreError> for OutboxProcessError {
    fn from(value: StoreError) -> Self {
        Self::Retryable(value.into())
    }
}

impl From<serde_json::Error> for OutboxProcessError {
    fn from(value: serde_json::Error) -> Self {
        Self::Retryable(value.into())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntryOutboxPayload {
    #[serde(alias = "journal_id")]
    pub(crate) journal_id: String,
    #[serde(alias = "entry_id")]
    pub(crate) entry_id: String,
    #[serde(default, alias = "queued_at")]
    pub(crate) queued_at: Option<String>,
    #[serde(default, alias = "queued_at_epoch_ms")]
    pub(crate) queued_at_epoch_ms: Option<i64>,
    #[serde(default, alias = "edit_date_epoch_ms")]
    pub(crate) edit_date_epoch_ms: Option<i64>,
    #[serde(default, alias = "entry_json")]
    pub(crate) entry_json: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OriginalMediaOutboxPayload {
    #[serde(alias = "journal_id")]
    pub(crate) journal_id: String,
    #[serde(alias = "entry_id")]
    pub(crate) entry_id: String,
    #[serde(alias = "moment_id")]
    pub(crate) moment_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommentOutboxPayload {
    pub(crate) id: String,
    #[serde(alias = "journal_id")]
    pub(crate) journal_id: String,
    #[serde(alias = "entry_id")]
    pub(crate) entry_id: String,
    #[serde(default)]
    pub(crate) content: Option<String>,
}

mod core;
mod helpers;

#[allow(unused_imports)]
pub(crate) use core::{
    drain_sync_outbox, process_outbox_item, push_context_item_outbox, push_daily_chat_feed_outbox,
    push_daily_chat_settings_outbox, push_entry_outbox, push_journal_create_outbox,
};
#[allow(unused_imports)]
pub(crate) use helpers::{
    build_entry_content_from_local, build_entry_envelope_moments, build_entry_thumbnail_parts,
    build_outbox_crypto_context, datetime_to_epoch_ms, decode_comment_outbox_payload,
    decode_context_item_outbox_payload, decode_daily_chat_feed_outbox_payload,
    decode_daily_chat_settings_outbox_payload, decode_entry_outbox_payload,
    decode_journal_create_outbox_payload, decode_original_media_outbox_payload,
    entry_date_value_to_f64, mark_retry_or_failed, normalize_entry_content_for_push,
    parse_entry_put_response_bytes, push_original_media_outbox, queued_entry_edit_date_epoch_ms,
    reset_current_outbox_lease, resolve_outbox_edit_date_epoch_ms, resolve_sync_upload_base_url,
    retry_delay_ms, should_defer_outbox_item_until_keys, value_to_string,
};
