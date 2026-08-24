use std::collections::HashMap;
use std::io::Read;

// ponytail: provisional compatibility ceiling; replace with the server's authoritative limit when one exists.
const MAX_DECRYPTED_ENTRY_BYTES: u64 = 128 * 1024 * 1024;

use aes_gcm::{Aes256Gcm, aead::Aead, aead::KeyInit};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use flate2::read::GzDecoder;
use md5::Md5;
use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey, EncodeRsaPrivateKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
use serde_json::Value;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::entry_embeddings::{
    compute_embedding_for_searchable_text, entry_searchable_text, source_hash_for_searchable_text,
};
use crate::http::DayOneApiClient;
use crate::models::{Entry, EntryId, Journal, JournalId, Moment};
use crate::store::sqlite::{JournalSyncStatus, Store};
use crate::store::{JournalMetadataDiagnostic, SyncResourceRunSave};
use crate::sync::conflicts::local_is_newer;
use crate::sync::crypto::{
    UserKeysPayload, build_crypto_context_from_sources, decrypt_payload,
    derive_d1_symmetric_key_from_master_key,
};
use crate::sync::engine::{MAX_PAGE_LOOPS, ResourceSyncOutput, log_sync, now_epoch_ms};
#[derive(Debug, Clone)]
pub(crate) struct JournalDecryptor {
    user_id: Option<String>,
    user_private_key: RsaPrivateKey,
    user_verify_keys: HashMap<String, RsaPublicKey>,
}

#[derive(Debug, Clone)]
pub(crate) struct EntriesDecryptor {
    journal_keys_by_fingerprint: HashMap<String, RsaPrivateKey>,
}

#[derive(Debug, Clone, Copy)]
struct CursorResourceParams<'a> {
    resource_key: &'a str,
    api_path: &'a str,
    static_params: &'a [(String, String)],
}

struct EntriesFeedResourceParams<'a> {
    resource_key: &'a str,
    api_path: &'a str,
    static_params: &'a [(String, String)],
    entries_decryptor: Option<&'a EntriesDecryptor>,
    embeddings_enabled: &'a mut bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FeatureFlags {
    pub(crate) daily_chat: bool,
    pub(crate) context_library: bool,
    pub(crate) journal_hierarchy: bool,
}

impl FeatureFlags {
    pub(crate) fn from_payload(feature_flags: &Value) -> Self {
        Self {
            journal_hierarchy: feature_enabled(
                feature_flags,
                &[
                    "journal_collections",
                    "enable_journal_collections",
                    "journal_hierarchy",
                ],
            ),
            daily_chat: feature_enabled(
                feature_flags,
                &[
                    "enable_daily_chat",
                    "daily_chat",
                    "ai-daily-chat",
                    "aiDailyChat",
                    "daily-chat",
                    "daily-chat-memories",
                ],
            ),
            context_library: feature_enabled(
                feature_flags,
                &[
                    "enable_daily_chat_memories",
                    "daily-chat-memories",
                    "context_library",
                    "memories",
                ],
            ),
        }
    }
}
#[derive(Debug)]
pub(crate) struct FeedRecordWithPayload {
    pub(crate) record: Entry,
    pub(crate) payload_bytes: Option<Vec<u8>>,
}

pub(crate) mod crypto;
mod entries;
mod phase;
mod resources;

#[allow(unused_imports)]
pub(crate) use entries::{
    is_deleted_journal_entries_feed_error, merge_payload_value, parse_entries_feed_bytes,
};
pub(crate) use phase::{fetch_pre_sync_resources, run_pull_phase};
#[allow(unused_imports)]
pub(crate) use resources::{
    RequiredSingletonErrorKind, build_journals_sync_params, extract_items, feature_enabled,
    value_id, value_to_string,
};
