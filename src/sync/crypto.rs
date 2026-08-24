use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Write};

use aes_gcm::{Aes256Gcm, aead::Aead, aead::KeyInit};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use flate2::read::GzDecoder;
use flate2::{Compression, write::GzEncoder};
use md5::{Digest, Md5};
use pbkdf2::pbkdf2_hmac_array;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::rand_core::{OsRng, RngCore};
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
use serde_json::Value;
use sha1::Sha1;
use sha2::Sha256;

#[derive(Debug)]
struct MissingActiveContentPublicKeyError {
    message: String,
}

impl fmt::Display for MissingActiveContentPublicKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MissingActiveContentPublicKeyError {}

/// Local key material needed to encrypt an E2E journal object is unavailable
/// or unusable right now. This intentionally covers both truly-missing inputs
/// (no profile key / no synced user keys) and key-resolution failures (bad key,
/// stale grants, corrupt local key payload). Outbox handling treats this class
/// as deferable so user data is never dead-lettered just because local E2E key
/// state is temporarily or administratively broken.
#[derive(Debug)]
struct E2eKeyMaterialUnavailableError {
    reason: String,
}

impl fmt::Display for E2eKeyMaterialUnavailableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E2E key material unavailable: {}", self.reason)
    }
}

impl std::error::Error for E2eKeyMaterialUnavailableError {}

fn e2e_key_material_unavailable_error(reason: impl Into<String>) -> anyhow::Error {
    anyhow!(E2eKeyMaterialUnavailableError {
        reason: reason.into(),
    })
}

#[derive(Debug, Clone, Default)]
pub struct CryptoContext {
    pub master_key: Option<String>,
    d1_symmetric_key: Option<[u8; 32]>,
    d1_content_private_keys: HashMap<String, RsaPrivateKey>,
    active_content_public_key: Option<(String, RsaPublicKey)>,
    active_content_public_key_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct UserKeysPayload {
    encrypted_private_key: Option<String>,
    user_public_key: Option<String>,
    user_key_fingerprint: Option<String>,
    alternative_user_keys: Vec<UserKeysPayload>,
    active_content_key_fingerprint: Option<String>,
    has_content_keys_field: bool,
    content_keys: Vec<UserContentKey>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct UserContentKey {
    fingerprint: Option<String>,
    encrypted_private_key: Option<String>,
    public_key: Option<String>,
}

impl UserKeysPayload {
    pub(crate) fn from_raw(raw: &Value) -> Option<Self> {
        let blob_json = parse_user_keys_blob_json(raw)?;
        let root_user_key = raw.get("userKey").or_else(|| raw.get("user_key"));
        let blob_user_key = blob_json
            .as_ref()
            .and_then(|v| v.get("userKey").or_else(|| v.get("user_key")));
        let canonical_root = blob_json.as_ref().unwrap_or(raw);
        let canonical_user_key = canonical_root
            .get("userKey")
            .or_else(|| canonical_root.get("user_key"));

        let encrypted_private_key = [
            blob_user_key.and_then(|v| v.get("encryptedPrivateKey")),
            blob_user_key.and_then(|v| v.get("encrypted_private_key")),
            blob_json
                .as_ref()
                .and_then(|v| v.get("encryptedPrivateKey")),
            blob_json
                .as_ref()
                .and_then(|v| v.get("encrypted_private_key")),
            root_user_key.and_then(|v| v.get("encryptedPrivateKey")),
            root_user_key.and_then(|v| v.get("encrypted_private_key")),
            raw.get("encryptedPrivateKey"),
            raw.get("encrypted_private_key"),
        ]
        .into_iter()
        .flatten()
        .find_map(|v| v.as_str().map(ToOwned::to_owned));

        let user_public_key = canonical_user_key
            .and_then(|v| v.get("publicKey").or_else(|| v.get("public_key")))
            .or_else(|| {
                canonical_root
                    .get("publicKey")
                    .or_else(|| canonical_root.get("public_key"))
            })
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let user_key_fingerprint = canonical_user_key
            .and_then(|v| v.get("fingerprint"))
            .or_else(|| canonical_root.get("fingerprint"))
            .and_then(Value::as_str)
            .map(|s| s.to_ascii_lowercase());

        let mut alternative_user_keys = Vec::new();
        for field in [
            "otherUserKeys",
            "other_user_keys",
            "alternativeUserKeys",
            "alternative_user_keys",
        ] {
            if let Some(keys) = canonical_root.get(field).and_then(Value::as_array) {
                alternative_user_keys.extend(keys.iter().filter_map(Self::from_raw));
            }
        }

        let active_content_key_fingerprint = [
            canonical_root.get("activeContentKeyFingerprint"),
            canonical_root.get("active_content_key_fingerprint"),
        ]
        .into_iter()
        .flatten()
        .find_map(|v| v.as_str().map(|s| s.to_ascii_lowercase()));

        let content_keys_field = canonical_root
            .get("contentKeys")
            .or_else(|| canonical_root.get("content_keys"));
        let has_content_keys_field = content_keys_field.is_some();
        let content_keys = content_keys_field
            .and_then(Value::as_array)
            .map(|keys| {
                keys.iter()
                    .map(|key| UserContentKey {
                        fingerprint: key
                            .get("fingerprint")
                            .and_then(Value::as_str)
                            .map(|s| s.to_ascii_lowercase()),
                        encrypted_private_key: [
                            key.get("encryptedPrivateKey"),
                            key.get("encrypted_private_key"),
                        ]
                        .into_iter()
                        .flatten()
                        .find_map(|v| v.as_str().map(ToOwned::to_owned)),
                        public_key: [key.get("publicKey"), key.get("public_key")]
                            .into_iter()
                            .flatten()
                            .find_map(|v| v.as_str().map(ToOwned::to_owned)),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Some(Self {
            encrypted_private_key,
            user_public_key,
            user_key_fingerprint,
            alternative_user_keys,
            active_content_key_fingerprint,
            has_content_keys_field,
            content_keys,
        })
    }

    pub(crate) fn encrypted_private_key(&self) -> Option<&str> {
        self.encrypted_private_key.as_deref()
    }

    pub(crate) fn user_public_key(&self) -> Option<&str> {
        self.user_public_key.as_deref()
    }

    pub(crate) fn user_key_fingerprint(&self) -> Option<&str> {
        self.user_key_fingerprint.as_deref()
    }

    pub(crate) fn alternative_user_keys(&self) -> &[UserKeysPayload] {
        &self.alternative_user_keys
    }

    pub(crate) fn active_content_key_fingerprint(&self) -> Option<&str> {
        self.active_content_key_fingerprint.as_deref()
    }

    pub(crate) fn content_keys(&self) -> &[UserContentKey] {
        &self.content_keys
    }

    pub(crate) fn has_content_keys_field(&self) -> bool {
        self.has_content_keys_field
    }

    pub(crate) fn active_content_key(&self) -> Option<&UserContentKey> {
        let active_fp = self.active_content_key_fingerprint()?;
        self.content_keys.iter().find(|key| {
            key.fingerprint()
                .map(|fp| fp.eq_ignore_ascii_case(active_fp))
                .unwrap_or(false)
        })
    }
}

impl UserContentKey {
    pub(crate) fn fingerprint(&self) -> Option<&str> {
        self.fingerprint.as_deref()
    }

    pub(crate) fn encrypted_private_key(&self) -> Option<&str> {
        self.encrypted_private_key.as_deref()
    }

    pub(crate) fn public_key(&self) -> Option<&str> {
        self.public_key.as_deref()
    }
}

#[cfg(test)]
pub fn build_crypto_context(
    master_key: Option<String>,
    user_keys_json: Option<&str>,
) -> CryptoContext {
    build_crypto_context_from_sources(master_key, user_keys_json, user_keys_json)
}

pub fn build_crypto_context_from_sources(
    master_key: Option<String>,
    user_identity_key_json: Option<&str>,
    content_key_bundle_json: Option<&str>,
) -> CryptoContext {
    let user_identity_key_json = user_identity_key_json.or(content_key_bundle_json);
    let content_key_bundle_json = content_key_bundle_json.or(user_identity_key_json);
    let d1_symmetric_key = master_key
        .as_deref()
        .and_then(derive_d1_symmetric_key_from_master_key);
    let d1_content_private_keys = match (
        user_identity_key_json,
        content_key_bundle_json,
        d1_symmetric_key.as_ref(),
    ) {
        (Some(user_key), Some(content_keys), Some(sym_key)) => {
            load_content_private_keys(user_key, content_keys, sym_key)
        }
        _ => HashMap::new(),
    };
    let active_content_public_key = load_active_content_public_key(content_key_bundle_json);
    let active_content_public_key_error = if active_content_public_key.is_none() {
        Some(active_content_key_error_message(content_key_bundle_json))
    } else {
        None
    };
    CryptoContext {
        master_key,
        d1_symmetric_key,
        d1_content_private_keys,
        active_content_public_key,
        active_content_public_key_error,
    }
}

pub fn decrypt_payload(payload: &Value, ctx: &CryptoContext) -> Result<Value> {
    decrypt_payload_owned(payload.clone(), ctx)
}

pub fn decrypt_payload_owned(payload: Value, ctx: &CryptoContext) -> Result<Value> {
    if ctx.master_key.is_none() {
        return Ok(payload);
    }
    let mut value = payload;
    decrypt_daily_chat_payload_in_place(&mut value, ctx);
    Ok(decrypt_value(value, ctx))
}

pub fn encrypt_payload(payload: Value, ctx: &CryptoContext) -> Result<Value> {
    if !is_daily_chat_payload(&payload) {
        return Ok(payload);
    }
    encrypt_daily_chat_payload(payload, ctx)
}

pub fn encrypt_context_library_payload(payload: Value, ctx: &CryptoContext) -> Result<Value> {
    let mut item = payload
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("context library payload must be an object"))?;
    let (fingerprint_hex, content_public_key) = ctx
        .active_content_public_key
        .as_ref()
        .ok_or_else(|| {
            anyhow!(MissingActiveContentPublicKeyError {
                message: ctx.active_content_public_key_error.as_deref().unwrap_or(
                    "missing active content key for context library encryption; run `dayone sync` to refresh user keys"
                )
                .to_owned()
            })
        })?;

    for field in ["content", "category", "tags"] {
        let Some(value) = item.get(field).cloned() else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        if let Value::String(existing) = &value
            && is_d1_base64_payload(existing)
        {
            continue;
        }
        let plaintext = match value {
            Value::String(text) => text,
            other => serde_json::to_string(&other)?,
        };
        let encrypted = encrypt_d1_mode1_with_public_key(
            plaintext.as_bytes(),
            fingerprint_hex,
            content_public_key,
        )?;
        item.insert(
            field.to_owned(),
            Value::String(BASE64_STANDARD.encode(encrypted)),
        );
    }

    Ok(Value::Object(item))
}

pub fn is_missing_active_content_public_key_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<MissingActiveContentPublicKeyError>()
        .is_some()
}

pub fn is_e2e_key_material_unavailable_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<E2eKeyMaterialUnavailableError>()
        .is_some()
}

#[derive(Debug)]
pub struct EntryEncryptionResult {
    pub field_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

pub fn encrypt_entry_content_for_journal(
    journal: &Value,
    entry_content: &Value,
    master_key: Option<&str>,
    user_keys_json: Option<&str>,
) -> Result<EntryEncryptionResult> {
    let Some((fingerprint_hex, content_public_key)) =
        resolve_journal_content_public_key(journal, master_key, user_keys_json)?
    else {
        return Ok(EntryEncryptionResult {
            field_name: "content".to_owned(),
            mime_type: "application/json".to_owned(),
            bytes: serde_json::to_vec(entry_content)?,
        });
    };

    let payload_bytes = serde_json::to_vec(entry_content)?;
    let encrypted =
        encrypt_entry_payload_d1_mode2(&payload_bytes, &fingerprint_hex, &content_public_key)
            .map_err(|err| {
                e2e_key_material_unavailable_error(format!(
                    "entry content could not be encrypted with the journal key: {err}"
                ))
            })?;
    Ok(EntryEncryptionResult {
        field_name: "encrypted-content".to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        bytes: encrypted,
    })
}

/// Returns `true` when the journal is end-to-end encrypted (carries an
/// `encryption.vault`). Plaintext journals leave media bytes unencrypted.
pub fn journal_is_e2e_encrypted(journal: &Value) -> bool {
    journal
        .get("encryption")
        .and_then(|v| v.get("vault"))
        .and_then(Value::as_object)
        .is_some()
}

/// Client-side encrypts original media bytes for an end-to-end encrypted
/// journal, mirroring the web client's media crypto: a D1 format-1 ("locked
/// key") blob whose per-object AES-256-GCM content key is RSA-OAEP-SHA1 wrapped
/// to the journal content key, with the journal-key fingerprint embedded. The
/// returned bytes are what should be uploaded to S3 so the server only ever
/// sees ciphertext.
///
/// Media bytes are binary, so (unlike JSON entry content) they are *not*
/// gzip-compressed before encryption — format 1, not format 2.
///
/// The caller must ensure the journal is E2E encrypted
/// (see [`journal_is_e2e_encrypted`]); a plaintext journal yields an error.
pub fn encrypt_original_media_for_journal(
    journal: &Value,
    media_bytes: &[u8],
    master_key: Option<&str>,
    user_keys_json: Option<&str>,
) -> Result<Vec<u8>> {
    let (fingerprint_hex, content_public_key) =
        resolve_journal_content_public_key(journal, master_key, user_keys_json)?.ok_or_else(
            || anyhow!("cannot client-side encrypt media for a non-encrypted journal"),
        )?;
    encrypt_d1_mode1_with_public_key(media_bytes, &fingerprint_hex, &content_public_key).map_err(
        |err| {
            e2e_key_material_unavailable_error(format!(
                "original media could not be encrypted with the journal key: {err}"
            ))
        },
    )
}

/// Unlocks the journal content public key (and its fingerprint) used to wrap
/// per-object content keys for E2E journals. Returns `Ok(None)` for plaintext
/// journals (no `encryption.vault`).
fn resolve_journal_content_public_key(
    journal: &Value,
    master_key: Option<&str>,
    user_keys_json: Option<&str>,
) -> Result<Option<(String, RsaPublicKey)>> {
    let Some(vault) = journal
        .get("encryption")
        .and_then(|v| v.get("vault"))
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let vault_key = resolve_journal_vault_key(vault, master_key, user_keys_json)?;
    let (fingerprint_hex, content_public_key) = load_journal_content_public_key(vault, &vault_key)
        .map_err(|err| {
            e2e_key_material_unavailable_error(format!(
                "journal content public key could not be loaded: {err}"
            ))
        })?;
    Ok(Some((fingerprint_hex, content_public_key)))
}

/// Recovers the 32-byte journal vault key by RSA-OAEP-unwrapping the active
/// user's grant inside the journal's `encryption.vault`. The caller checks for
/// vault presence (a missing vault means a plaintext journal); this errors if
/// the journal is E2E but the master key / synced user keys are missing or no
/// grant unlocks.
fn resolve_journal_vault_key(
    vault: &serde_json::Map<String, Value>,
    master_key: Option<&str>,
    user_keys_json: Option<&str>,
) -> Result<Vec<u8>> {
    let master_key = master_key.ok_or_else(|| {
        e2e_key_material_unavailable_error("profile encryption key is not configured")
    })?;
    let user_keys_json = user_keys_json.ok_or_else(|| {
        e2e_key_material_unavailable_error("synced user keys are not available locally")
    })?;
    let encrypted_private_key = parse_user_keys_payload(user_keys_json)
        .and_then(|payload| payload.encrypted_private_key().map(ToOwned::to_owned))
        .ok_or_else(|| {
            e2e_key_material_unavailable_error(
                "synced user-key payload does not contain an encrypted private key",
            )
        })?;
    let derived = derive_d1_symmetric_key_from_master_key(master_key).ok_or_else(|| {
        e2e_key_material_unavailable_error("profile encryption key is not a valid D1 master key")
    })?;
    let user_private_key_bytes = decrypt_d1_mode0_with_key(&encrypted_private_key, &derived)
        .map_err(|err| {
            e2e_key_material_unavailable_error(format!(
                "synced user private key could not be unlocked with the profile encryption key: {err}"
            ))
        })?;
    let user_private_key = parse_private_key_bytes(&user_private_key_bytes).map_err(|err| {
        e2e_key_material_unavailable_error(format!(
            "unlocked user private key could not be parsed: {err}"
        ))
    })?;
    let vault_key = decrypt_vault_key(vault, &user_private_key)
        .map_err(|err| {
            e2e_key_material_unavailable_error(format!(
                "journal vault key grants could not be decoded: {err}"
            ))
        })?
        .ok_or_else(|| {
            e2e_key_material_unavailable_error(
                "journal vault key could not be unlocked from local grants",
            )
        })?;
    Ok(vault_key)
}

/// Journal metadata fields an E2E journal stores D1-encrypted under the vault
/// key (matching the web client). Plaintext journals send them verbatim.
pub(crate) const ENCRYPTED_JOURNAL_METADATA_FIELDS: [&str; 2] = ["name", "description_v2"];

/// D1-encrypts the journal's `name`/`description_v2` in a server-bound `payload`
/// under the vault key, matching the web client (`encryptD1NoLockedKey`).
///
/// Called at PUSH time only, so the local journal row keeps human-readable
/// metadata (the display source of truth for the TUI / `list journals`) — the
/// same shape as entry content, which is encrypted on the way out and never
/// stored encrypted locally. The vault key is recovered from the payload's own
/// `encryption` vault.
///
/// No-op for a plaintext journal, or for fields that are absent, empty, or
/// already D1-encoded (the `is_d1_base64_payload` guard keeps it idempotent, so
/// a re-pushed already-encrypted value is never double-encrypted).
pub(crate) fn encrypt_journal_metadata_for_push(
    payload: &mut serde_json::Map<String, Value>,
    master_key: Option<&str>,
    user_keys_json: Option<&str>,
) -> Result<()> {
    // Scope the immutable borrow of `payload` so we can mutate it below; the
    // recovered key is owned. A missing vault means a plaintext journal -> no-op.
    let vault_key_bytes = {
        let Some(vault) = payload
            .get("encryption")
            .and_then(|v| v.get("vault"))
            .and_then(Value::as_object)
        else {
            return Ok(());
        };
        resolve_journal_vault_key(vault, master_key, user_keys_json)?
    };
    let vault_key: [u8; 32] = vault_key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| e2e_key_material_unavailable_error("journal vault key must be 32 bytes"))?;

    for field in ENCRYPTED_JOURNAL_METADATA_FIELDS {
        let Some(Value::String(plaintext)) = payload.get(field) else {
            continue;
        };
        if plaintext.is_empty() || is_d1_base64_payload(plaintext) {
            continue;
        }
        let encrypted =
            encrypt_d1_mode0_with_key(plaintext.as_bytes(), &vault_key).map_err(|err| {
                e2e_key_material_unavailable_error(format!(
                    "journal metadata field '{field}' could not be encrypted: {err}"
                ))
            })?;
        payload.insert(field.to_owned(), Value::String(encrypted));
    }
    Ok(())
}

fn decrypt_vault_key(
    vault: &serde_json::Map<String, Value>,
    user_private_key: &RsaPrivateKey,
) -> Result<Option<Vec<u8>>> {
    let Some(grants) = vault.get("grants").and_then(Value::as_array) else {
        return Ok(None);
    };
    for grant in grants {
        let Some(encrypted_vault_key) = grant.get("encrypted_vault_key").and_then(Value::as_str)
        else {
            continue;
        };
        let encrypted = BASE64_STANDARD.decode(encrypted_vault_key)?;
        if let Ok(vault_key) = user_private_key.decrypt(Oaep::new::<Sha1>(), &encrypted) {
            return Ok(Some(vault_key));
        }
    }
    Ok(None)
}

fn load_journal_content_public_key(
    vault: &serde_json::Map<String, Value>,
    vault_key: &[u8],
) -> Result<(String, rsa::RsaPublicKey)> {
    let keys = vault
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("journal vault keys missing"))?;
    let key = keys
        .first()
        .ok_or_else(|| anyhow!("journal vault keys are empty"))?;
    let fingerprint = key
        .get("fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("journal key fingerprint missing"))?
        .to_ascii_lowercase();
    let encrypted_private_key = key
        .get("encrypted_private_key")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("journal encrypted private key missing"))?;
    let private_key_bytes = decrypt_d1_mode0_with_key(encrypted_private_key, vault_key)?;
    let private_key = parse_private_key_bytes(&private_key_bytes)?;
    Ok((fingerprint, private_key.to_public_key()))
}

fn encrypt_entry_payload_d1_mode2(
    plaintext_json: &[u8],
    fingerprint_hex: &str,
    content_public_key: &rsa::RsaPublicKey,
) -> Result<Vec<u8>> {
    let fingerprint_bytes = decode_hex_to_bytes(fingerprint_hex)?;
    if fingerprint_bytes.len() != 32 {
        bail!("journal key fingerprint must decode to 32 bytes");
    }

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(plaintext_json)?;
    let compressed = gz.finish()?;

    let mut rng = OsRng;
    let mut content_key = [0_u8; 32];
    rng.fill_bytes(&mut content_key);
    let mut iv = [0_u8; 12];
    rng.fill_bytes(&mut iv);

    let key = aes_gcm::Key::<Aes256Gcm>::try_from(content_key.as_slice())?;
    let cipher = Aes256Gcm::new(&key);
    let ciphertext_and_tag = cipher
        .encrypt((&iv).into(), compressed.as_slice())
        .map_err(|_| anyhow!("failed to encrypt entry content"))?;
    let wrapped_key = content_public_key.encrypt(&mut rng, Oaep::new::<Sha1>(), &content_key)?;

    let mut out = Vec::new();
    out.extend_from_slice(b"D1");
    out.push(1); // schema
    out.push(2); // mode 2 (gzip payload)
    out.extend_from_slice(&fingerprint_bytes);
    out.extend_from_slice(&[0_u8, 0_u8]); // signature length = 0
    out.extend_from_slice(&wrapped_key);
    out.extend_from_slice(&iv);
    out.extend_from_slice(&ciphertext_and_tag);

    let mut md5 = Md5::new();
    md5.update(&out);
    out.extend_from_slice(&md5.finalize());
    Ok(out)
}

fn decode_hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(hex.len() / 2);
    let clean = hex.trim();
    if !clean.len().is_multiple_of(2) {
        bail!("hex string must have even length");
    }
    let bytes = clean.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        let hi = hex_nibble(bytes[idx])?;
        let lo = hex_nibble(bytes[idx + 1])?;
        out.push((hi << 4) | lo);
        idx += 2;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex character"),
    }
}

fn decrypt_value(value: Value, ctx: &CryptoContext) -> Value {
    match value {
        Value::String(s) => maybe_decrypt_string_value(&s, ctx),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(|v| decrypt_value(v, ctx)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, decrypt_value(v, ctx)))
                .collect(),
        ),
        other => other,
    }
}

fn maybe_decrypt_string_value(input: &str, ctx: &CryptoContext) -> Value {
    // Some server/docs examples encode encrypted values as ENCRYPTED(...)
    // wrappers. Normalize these to plaintext payload when possible.
    if input.starts_with("ENCRYPTED(") && input.ends_with(')') {
        let inner = &input["ENCRYPTED(".len()..input.len() - 1];
        return Value::String(inner.to_owned());
    }

    if input.starts_with("RD")
        && let Ok(bytes) = decrypt_d1_with_context(input, ctx)
        && let Ok(text) = String::from_utf8(bytes)
    {
        let trimmed = text.trim();
        if (trimmed.starts_with('[') || trimmed.starts_with('{'))
            && let Ok(parsed) = serde_json::from_str::<Value>(trimmed)
        {
            return parsed;
        }
        return Value::String(text);
    }

    Value::String(input.to_owned())
}

pub(crate) fn derive_d1_symmetric_key_from_master_key(master_key: &str) -> Option<[u8; 32]> {
    let parts: Vec<&str> = master_key.split('-').collect();
    if parts.len() < 3 || !parts[0].eq_ignore_ascii_case("D1") {
        return None;
    }
    let user_id = parts[1];
    let password = parts[2..].join("");
    Some(pbkdf2_hmac_array::<Sha256, 32>(
        password.as_bytes(),
        user_id.as_bytes(),
        100_000,
    ))
}

fn load_content_private_keys(
    user_identity_key_json: &str,
    content_key_bundle_json: &str,
    symmetric_key: &[u8; 32],
) -> HashMap<String, RsaPrivateKey> {
    let mut map = HashMap::new();
    let Some(user_identity_key) = parse_user_keys_payload(user_identity_key_json) else {
        return map;
    };

    // First unlock the user's private key with the master-derived symmetric key.
    let Some(user_encrypted_private_key) = user_identity_key.encrypted_private_key() else {
        return map;
    };
    let Ok(user_private_key_bytes) =
        decrypt_d1_mode0_with_key(user_encrypted_private_key, symmetric_key)
    else {
        return map;
    };
    let Ok(user_private_key) = parse_private_key_bytes(&user_private_key_bytes) else {
        return map;
    };

    if let Some(user_fp) = user_identity_key.user_key_fingerprint() {
        map.insert(user_fp.to_owned(), user_private_key.clone());
    }

    // The supplementary bundle is optional. A malformed bundle must not make
    // the independently valid user identity key disappear from the context.
    let Some(content_key_bundle) = parse_user_keys_payload(content_key_bundle_json) else {
        return map;
    };
    if content_key_bundle.content_keys().is_empty() {
        return map;
    }

    for content_key in content_key_bundle.content_keys() {
        let Some(fingerprint) = content_key.fingerprint() else {
            continue;
        };
        let Some(encrypted_private_key) = content_key.encrypted_private_key() else {
            continue;
        };

        // Content private keys are D1 with locked key (format 1), encrypted
        // to the user's public key. Decrypt with the unlocked user private key.
        let Ok(private_key_bytes) =
            decrypt_d1_mode1_with_rsa_key(encrypted_private_key, &user_private_key)
        else {
            continue;
        };
        let Ok(private_key) = parse_private_key_bytes(&private_key_bytes) else {
            continue;
        };
        map.insert(fingerprint.to_owned(), private_key);
    }

    map
}

fn load_active_content_public_key(user_keys_json: Option<&str>) -> Option<(String, RsaPublicKey)> {
    let json = user_keys_json?;
    let payload = parse_user_keys_payload(json)?;
    let active_fingerprint = payload.active_content_key_fingerprint()?.to_owned();
    let active_key = payload.active_content_key()?;
    let public_key_pem = active_key.public_key()?;
    let public_key = parse_public_key_pem(public_key_pem).ok()?;
    Some((active_fingerprint, public_key))
}

fn parse_user_keys_payload(user_keys_json: &str) -> Option<UserKeysPayload> {
    let root: Value = serde_json::from_str(user_keys_json).ok()?;
    UserKeysPayload::from_raw(&root)
}

fn parse_user_keys_blob_json(raw: &Value) -> Option<Option<Value>> {
    let Some(blob) = raw.get("blob") else {
        // No blob means we can safely use top-level fields.
        return Some(None);
    };
    let blob_b64 = blob.as_str()?;
    let blob_bytes = BASE64_STANDARD.decode(blob_b64).ok()?;
    let blob_json = serde_json::from_slice::<Value>(&blob_bytes).ok()?;
    Some(Some(blob_json))
}

fn active_content_key_error_message(user_keys_json: Option<&str>) -> String {
    let Some(json) = user_keys_json else {
        return "missing synced user keys for daily chat encryption; run `dayone sync` to fetch user keys".to_owned();
    };
    let Some(user_keys_payload) = parse_user_keys_payload(json) else {
        return "failed to parse synced user keys for daily chat encryption; run `dayone sync` to refresh user keys".to_owned();
    };
    let active_fingerprint = user_keys_payload
        .active_content_key_fingerprint()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if active_fingerprint.is_none() {
        return "synced user keys are missing `activeContentKeyFingerprint`; run `dayone sync` to refresh keys".to_owned();
    }
    let keys = user_keys_payload.content_keys();
    if keys.is_empty() {
        if user_keys_payload.has_content_keys_field() {
            return "synced user keys contain no content keys; run `dayone sync` to refresh keys"
                .to_owned();
        }
        return "synced user keys are missing `contentKeys`; run `dayone sync` to refresh keys"
            .to_owned();
    }
    let wanted = active_fingerprint.unwrap();
    let Some(active_key) = keys.iter().find(|k| {
        k.fingerprint()
            .map(|fp| fp.eq_ignore_ascii_case(wanted))
            .unwrap_or(false)
    }) else {
        return "active content key fingerprint not found in synced content keys; run `dayone sync` to refresh keys".to_owned();
    };
    let public_key_pem = active_key
        .public_key()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if public_key_pem.is_none() {
        return "active content key is missing `publicKey`; run `dayone sync` to refresh keys"
            .to_owned();
    }
    "active content public key is invalid in synced user keys; run `dayone sync` to refresh keys"
        .to_owned()
}

fn decrypt_daily_chat_payload_in_place(value: &mut Value, ctx: &CryptoContext) {
    if !is_daily_chat_payload(value) {
        return;
    }
    let Some(chat) = value.as_object_mut() else {
        return;
    };

    let shared_key = chat
        .get("shared_message_key")
        .and_then(|v| decode_shared_key(v, ctx).ok());

    if let Some(shared_key_bytes) = shared_key.as_ref()
        && let Some(history) = chat.get_mut("chat_history").and_then(Value::as_array_mut)
    {
        for message in history {
            let Some(msg_obj) = message.as_object_mut() else {
                continue;
            };
            if msg_obj.get("role").and_then(Value::as_str) == Some("meta") {
                continue;
            }
            let Some(content) = msg_obj.get("content").and_then(Value::as_str) else {
                continue;
            };
            if !content.starts_with("RD") {
                continue;
            }

            let decrypted = decrypt_d1_mode0_with_key(content, shared_key_bytes)
                .or_else(|_| decrypt_d1_with_context(content, ctx));
            if let Ok(bytes) = decrypted
                && let Ok(text) = String::from_utf8(bytes)
            {
                msg_obj.insert("content".to_owned(), Value::String(text));
            }
        }
    }

    if let Some(context) = chat.get_mut("context").and_then(Value::as_object_mut) {
        for value in context.values_mut() {
            let Value::String(input) = value else {
                continue;
            };
            if !input.starts_with("RD") {
                continue;
            }
            if let Ok(bytes) = decrypt_d1_with_context(input, ctx)
                && let Ok(text) = String::from_utf8(bytes)
            {
                if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                    *value = parsed;
                } else {
                    *value = Value::String(text);
                }
            }
        }
    }
}

fn encrypt_daily_chat_payload(payload: Value, ctx: &CryptoContext) -> Result<Value> {
    let mut chat = payload
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("daily chat payload must be an object"))?;
    let (fingerprint_hex, content_public_key) = ctx
        .active_content_public_key
        .as_ref()
        .ok_or_else(|| {
            anyhow!(MissingActiveContentPublicKeyError {
                message: ctx.active_content_public_key_error.as_deref().unwrap_or(
                    "missing active content key for daily chat encryption; run `dayone sync` to fetch user keys"
                )
                .to_owned()
            })
        })?;

    let mut shared_key_bytes = if let Some(existing) = chat.get("shared_message_key") {
        match decode_shared_key(existing, ctx) {
            Ok(bytes) => bytes,
            Err(err) => {
                if has_encrypted_non_meta_messages(&chat) {
                    bail!(
                        "shared_message_key is invalid for encrypted daily chat history: {}",
                        err
                    );
                }
                new_random_shared_key()
            }
        }
    } else {
        if has_encrypted_non_meta_messages(&chat) {
            bail!("shared_message_key is required when daily chat history is already encrypted");
        }
        new_random_shared_key()
    };
    if shared_key_bytes.iter().all(|b| *b == 0) {
        if has_encrypted_non_meta_messages(&chat) {
            bail!("shared_message_key is all zeros for encrypted daily chat history");
        }
        shared_key_bytes = new_random_shared_key();
    }

    let encrypted_shared_key =
        encrypt_d1_mode1_with_public_key(&shared_key_bytes, fingerprint_hex, content_public_key)?;
    chat.insert(
        "shared_message_key".to_owned(),
        Value::String(BASE64_STANDARD.encode(encrypted_shared_key)),
    );

    if let Some(history) = chat.get_mut("chat_history").and_then(Value::as_array_mut) {
        for message in history {
            let Some(msg_obj) = message.as_object_mut() else {
                continue;
            };
            if msg_obj.get("role").and_then(Value::as_str) == Some("meta") {
                continue;
            }
            let Some(content) = msg_obj.get("content").and_then(Value::as_str) else {
                continue;
            };
            if is_d1_base64_payload(content) {
                if decrypt_d1_mode0_with_key(content, &shared_key_bytes).is_ok() {
                    continue;
                }
                bail!(
                    "daily chat message content is encrypted with a mismatched shared_message_key"
                );
            }
            let encrypted_content =
                encrypt_d1_mode0_with_key(content.as_bytes(), &shared_key_bytes)?;
            msg_obj.insert("content".to_owned(), Value::String(encrypted_content));
        }
    }

    if let Some(context) = chat.get_mut("context").and_then(Value::as_object_mut) {
        for value in context.values_mut() {
            if value.is_null() {
                continue;
            }
            if let Value::String(existing) = value
                && is_d1_base64_payload(existing)
            {
                continue;
            }
            let plaintext = serde_json::to_string(value)?;
            let encrypted = encrypt_d1_mode1_with_public_key(
                plaintext.as_bytes(),
                fingerprint_hex,
                content_public_key,
            )?;
            *value = Value::String(BASE64_STANDARD.encode(encrypted));
        }
    }

    Ok(Value::Object(chat))
}

fn is_daily_chat_payload(value: &Value) -> bool {
    value
        .as_object()
        .map(|obj| obj.contains_key("date") && obj.contains_key("chat_history"))
        .unwrap_or(false)
}

fn has_encrypted_non_meta_messages(chat: &serde_json::Map<String, Value>) -> bool {
    chat.get("chat_history")
        .and_then(Value::as_array)
        .map(|history| {
            history.iter().any(|message| {
                let Some(msg_obj) = message.as_object() else {
                    return false;
                };
                if msg_obj.get("role").and_then(Value::as_str) == Some("meta") {
                    return false;
                }
                msg_obj
                    .get("content")
                    .and_then(Value::as_str)
                    .map(is_d1_base64_payload)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn decode_shared_key(value: &Value, ctx: &CryptoContext) -> Result<[u8; 32]> {
    let input = value
        .as_str()
        .ok_or_else(|| anyhow!("shared message key must be a string"))?;
    if let Some(raw) = input.strip_prefix("dosk.") {
        let bytes = BASE64_STANDARD.decode(raw)?;
        return to_32_byte_array(&bytes);
    }
    if is_d1_base64_payload(input) {
        let bytes = decrypt_d1_with_context(input, ctx)?;
        return to_32_byte_array(&bytes);
    }
    let bytes = BASE64_STANDARD.decode(input)?;
    to_32_byte_array(&bytes)
}

fn is_d1_base64_payload(input: &str) -> bool {
    let Ok(decoded) = BASE64_STANDARD.decode(input) else {
        return false;
    };
    decoded.len() >= 4 && &decoded[0..2] == b"D1" && decoded[2] == 1
}

fn to_32_byte_array(bytes: &[u8]) -> Result<[u8; 32]> {
    if bytes.len() != 32 {
        bail!("expected 32-byte shared key, got {}", bytes.len());
    }
    let mut out = [0_u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

fn new_random_shared_key() -> [u8; 32] {
    let mut out = [0_u8; 32];
    let mut rng = OsRng;
    rng.fill_bytes(&mut out);
    out
}

fn decrypt_d1_with_context(input_b64: &str, ctx: &CryptoContext) -> Result<Vec<u8>> {
    let d1 = BASE64_STANDARD.decode(input_b64)?;
    if d1.len() < 4 {
        bail!("d1 payload too short");
    }
    if &d1[0..2] != b"D1" {
        bail!("invalid d1 header");
    }
    let schema = d1[2];
    if schema != 1 {
        bail!("unsupported d1 schema {}", schema);
    }
    let version = d1[3];
    match version {
        0 | 2 => {
            let Some(sym_key) = ctx.d1_symmetric_key.as_ref() else {
                bail!("no symmetric key available for d1 mode0");
            };
            decrypt_d1_mode0_with_key(input_b64, sym_key)
        }
        1 => decrypt_d1_mode1_with_private_keys(&d1, &ctx.d1_content_private_keys),
        _ => bail!("unsupported d1 format {}", version),
    }
}

fn decrypt_d1_mode1_with_private_keys(
    d1_payload: &[u8],
    private_keys: &HashMap<String, RsaPrivateKey>,
) -> Result<Vec<u8>> {
    if d1_payload.len() < 4 + 32 + 2 + 12 + 16 + 16 {
        bail!("d1 mode1 payload too short");
    }
    let mut offset = 4usize;
    let fingerprint_hex = bytes_to_hex_lower(&d1_payload[offset..offset + 32]);
    offset += 32;
    let sig_len = ((d1_payload[offset] as usize) << 8) | (d1_payload[offset + 1] as usize);
    offset += 2;
    let Some(private_key) = private_keys.get(&fingerprint_hex) else {
        bail!(
            "content private key not found for fingerprint {}",
            fingerprint_hex
        );
    };
    let wrapped_key_len = private_key.size();

    if d1_payload.len() < offset + sig_len + wrapped_key_len + 12 + 16 + 16 {
        bail!("d1 mode1 payload truncated");
    }
    offset += sig_len;
    let locked_key = &d1_payload[offset..offset + wrapped_key_len];
    offset += wrapped_key_len;
    let iv = &d1_payload[offset..offset + 12];
    offset += 12;
    let body_end = d1_payload.len() - 16; // strip checksum
    if body_end < offset {
        bail!("d1 mode1 body underflow");
    }
    let ciphertext_and_tag = &d1_payload[offset..body_end];

    let content_key = private_key.decrypt(Oaep::new::<Sha1>(), locked_key)?;
    if content_key.len() != 32 {
        bail!("unexpected content key size {}", content_key.len());
    }
    let key = aes_gcm::Key::<Aes256Gcm>::try_from(content_key.as_slice())?;
    let cipher = Aes256Gcm::new(&key);
    let mut decrypted = cipher
        .decrypt(iv.try_into()?, ciphertext_and_tag)
        .map_err(|_| anyhow!("failed to decrypt d1 mode1 payload"))?;

    if decrypted.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(decrypted.as_slice());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        decrypted = out;
    }
    Ok(decrypted)
}

fn decrypt_d1_mode1_with_rsa_key(input_b64: &str, private_key: &RsaPrivateKey) -> Result<Vec<u8>> {
    let d1_payload = BASE64_STANDARD.decode(input_b64)?;
    if d1_payload.len() < 4 + 32 + 2 + 12 + 16 + 16 {
        bail!("d1 mode1 payload too short");
    }
    if &d1_payload[0..2] != b"D1" {
        bail!("invalid d1 header");
    }
    if d1_payload[2] != 1 || d1_payload[3] != 1 {
        bail!("expected d1 format 1 for RSA decrypt");
    }

    let mut offset = 4usize;
    offset += 32; // fingerprint bytes
    let sig_len = ((d1_payload[offset] as usize) << 8) | (d1_payload[offset + 1] as usize);
    offset += 2;
    let wrapped_key_len = private_key.size();
    if d1_payload.len() < offset + sig_len + wrapped_key_len + 12 + 16 + 16 {
        bail!("d1 mode1 payload truncated");
    }
    offset += sig_len; // signature bytes
    let locked_key = &d1_payload[offset..offset + wrapped_key_len];
    offset += wrapped_key_len;
    let iv = &d1_payload[offset..offset + 12];
    offset += 12;
    let body_end = d1_payload.len() - 16; // strip checksum
    let ciphertext_and_tag = &d1_payload[offset..body_end];

    let content_key = private_key.decrypt(Oaep::new::<Sha1>(), locked_key)?;
    if content_key.len() != 32 {
        bail!("unexpected content key size {}", content_key.len());
    }
    let key = aes_gcm::Key::<Aes256Gcm>::try_from(content_key.as_slice())?;
    let cipher = Aes256Gcm::new(&key);
    let decrypted = cipher
        .decrypt(iv.try_into()?, ciphertext_and_tag)
        .map_err(|_| anyhow!("failed to decrypt d1 mode1 payload"))?;
    Ok(decrypted)
}

pub(crate) fn bytes_to_hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{:02x}", b));
    }
    out
}

fn parse_private_key_pem(pem: &str) -> Result<RsaPrivateKey> {
    if let Ok(pkcs8_key) = RsaPrivateKey::from_pkcs8_pem(pem) {
        return Ok(pkcs8_key);
    }
    Ok(RsaPrivateKey::from_pkcs1_pem(pem)?)
}

fn parse_public_key_pem(pem: &str) -> Result<RsaPublicKey> {
    if let Ok(pkcs8_key) = RsaPublicKey::from_public_key_pem(pem) {
        return Ok(pkcs8_key);
    }
    Ok(RsaPublicKey::from_pkcs1_pem(pem)?)
}

pub(crate) fn parse_private_key_bytes(bytes: &[u8]) -> Result<RsaPrivateKey> {
    if let Ok(pem) = std::str::from_utf8(bytes)
        && let Ok(key) = parse_private_key_pem(pem)
    {
        return Ok(key);
    }
    if let Ok(pkcs8_key) = RsaPrivateKey::from_pkcs8_der(bytes) {
        return Ok(pkcs8_key);
    }
    Ok(RsaPrivateKey::from_pkcs1_der(bytes)?)
}

fn encrypt_d1_mode1_with_public_key(
    plaintext: &[u8],
    fingerprint_hex: &str,
    public_key: &RsaPublicKey,
) -> Result<Vec<u8>> {
    let fingerprint_bytes = decode_hex_to_bytes(fingerprint_hex)?;
    if fingerprint_bytes.len() != 32 {
        bail!("content key fingerprint must decode to 32 bytes");
    }

    let payload = plaintext.to_vec();

    let mut rng = OsRng;
    let mut content_key = [0_u8; 32];
    rng.fill_bytes(&mut content_key);
    let mut iv = [0_u8; 12];
    rng.fill_bytes(&mut iv);

    let key = aes_gcm::Key::<Aes256Gcm>::try_from(content_key.as_slice())?;
    let cipher = Aes256Gcm::new(&key);
    let ciphertext_and_tag = cipher
        .encrypt((&iv).into(), payload.as_slice())
        .map_err(|_| anyhow!("failed to encrypt d1 mode1 payload"))?;
    let wrapped_key = public_key.encrypt(&mut rng, Oaep::new::<Sha1>(), &content_key)?;

    let mut out = Vec::new();
    out.extend_from_slice(b"D1");
    out.push(1); // schema
    out.push(1); // format 1 (locked-key D1)
    out.extend_from_slice(&fingerprint_bytes);
    out.extend_from_slice(&[0_u8, 0_u8]); // signature length = 0
    out.extend_from_slice(&wrapped_key);
    out.extend_from_slice(&iv);
    out.extend_from_slice(&ciphertext_and_tag);

    let mut md5 = Md5::new();
    md5.update(&out);
    out.extend_from_slice(&md5.finalize());
    Ok(out)
}

pub(crate) fn encrypt_d1_mode0_with_key(plaintext: &[u8], key_bytes: &[u8; 32]) -> Result<String> {
    let mut iv = [0_u8; 12];
    let mut rng = OsRng;
    rng.fill_bytes(&mut iv);

    let key = <&aes_gcm::Key<Aes256Gcm>>::try_from(key_bytes)?;
    let cipher = Aes256Gcm::new(key);
    let ciphertext_and_tag = cipher
        .encrypt((&iv).into(), plaintext)
        .map_err(|_| anyhow!("failed to encrypt d1 mode0 payload"))?;

    let mut out = Vec::new();
    out.extend_from_slice(b"D1");
    out.push(1); // schema
    out.push(0); // format 0
    out.extend_from_slice(&iv);
    out.extend_from_slice(&ciphertext_and_tag);

    let mut md5 = Md5::new();
    md5.update(&out);
    out.extend_from_slice(&md5.finalize());
    Ok(BASE64_STANDARD.encode(out))
}

pub(crate) fn decrypt_d1_mode0_with_key(input_b64: &str, key_bytes: &[u8]) -> Result<Vec<u8>> {
    let d1 = BASE64_STANDARD.decode(input_b64)?;
    if d1.len() < 4 + 12 + 16 + 16 {
        bail!("d1 payload too short");
    }
    if &d1[0..2] != b"D1" {
        bail!("invalid d1 header");
    }
    let schema = d1[2];
    if schema != 1 {
        bail!("unsupported d1 schema {}", schema);
    }
    let version = d1[3];
    if version != 0 && version != 2 {
        bail!("unsupported d1 format {} for symmetric decrypt", version);
    }

    let offset = 4;
    let iv = &d1[offset..offset + 12];
    let body_end = d1.len() - 16; // strip checksum
    let ciphertext_and_tag = &d1[offset + 12..body_end];
    let key = <&aes_gcm::Key<Aes256Gcm>>::try_from(key_bytes)?;
    let cipher = Aes256Gcm::new(key);
    let decrypted = cipher
        .decrypt(iv.try_into()?, ciphertext_and_tag)
        .map_err(|_| anyhow!("failed to decrypt d1 payload"))?;

    if version == 2 && decrypted.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(decrypted.as_slice());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        return Ok(out);
    }
    Ok(decrypted)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use rsa::RsaPrivateKey;
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use serde_json::json;

    pub(crate) const TEST_USER_ID: &str = "2468135790";
    pub(crate) const TEST_MASTER_KEY: &str = concat!(
        "D1-",
        "2468135790-",
        "ABCDEF-",
        "ABCDE-",
        "FGHIJ-",
        "KLMNO-",
        "PQRST-",
        "UVWXY"
    );

    pub(crate) struct E2eJournalFixture {
        pub(crate) journal: Value,
        pub(crate) master_key: String,
        pub(crate) user_keys_json: String,
        pub(crate) journal_private_key: RsaPrivateKey,
        pub(crate) fingerprint_hex: String,
    }

    /// Builds a fully-linked E2E journal vault and user-key payload so tests can
    /// exercise the real key hierarchy: master key → user key → vault key →
    /// journal content key.
    pub(crate) fn build_e2e_journal_fixture() -> E2eJournalFixture {
        let mut rng = OsRng;
        let derived = derive_d1_symmetric_key_from_master_key(TEST_MASTER_KEY)
            .expect("master key should derive symmetric key");

        let user_private =
            RsaPrivateKey::new(&mut rng, 2048).expect("user rsa key should generate");
        let user_public = user_private.to_public_key();
        // Stored as PKCS#1 PEM (text) — the pull-side decryptor unlocks the user
        // key via `String::from_utf8` + PEM parse, while the push side accepts
        // PEM-or-DER, so PEM keeps both paths working (and matches production).
        let user_private_pem = user_private
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .expect("user private key should PEM-encode");
        let user_encrypted_private_key =
            encrypt_d1_mode0_with_key(user_private_pem.as_bytes(), &derived)
                .expect("user private key should D1-encrypt");

        let journal_private =
            RsaPrivateKey::new(&mut rng, 2048).expect("journal rsa key should generate");
        let journal_private_der = journal_private
            .to_pkcs1_der()
            .expect("journal private key should encode")
            .as_bytes()
            .to_vec();

        let mut vault_key = [0_u8; 32];
        rng.fill_bytes(&mut vault_key);
        let encrypted_vault_key = user_public
            .encrypt(&mut rng, Oaep::new::<Sha1>(), &vault_key)
            .expect("vault key should wrap to user public key");

        let journal_encrypted_private_key =
            encrypt_d1_mode0_with_key(&journal_private_der, &vault_key)
                .expect("journal private key should D1-encrypt");
        let fingerprint_hex =
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_owned();

        let journal = json!({
            "id": "journal-e2e",
            "encryption": {
                "vault": {
                    "grants": [
                        { "encrypted_vault_key": BASE64_STANDARD.encode(&encrypted_vault_key) }
                    ],
                    "keys": [
                        {
                            "fingerprint": fingerprint_hex,
                            "encrypted_private_key": journal_encrypted_private_key
                        }
                    ]
                }
            }
        });

        let user_keys_json = json!({
            "userKey": {
                "fingerprint": "aa11",
                "encryptedPrivateKey": user_encrypted_private_key
            }
        })
        .to_string();

        E2eJournalFixture {
            journal,
            master_key: TEST_MASTER_KEY.to_owned(),
            user_keys_json,
            journal_private_key: journal_private,
            fingerprint_hex,
        }
    }

    /// Decrypts a client-side-encrypted media D1 blob using the journal content
    /// private key, mirroring what the pull side / web client do.
    pub(crate) fn decrypt_media_with_journal_key(
        blob: &[u8],
        journal_private_key: &RsaPrivateKey,
        fingerprint_hex: &str,
    ) -> Vec<u8> {
        let mut private_keys = HashMap::new();
        private_keys.insert(fingerprint_hex.to_owned(), journal_private_key.clone());
        decrypt_d1_mode1_with_private_keys(blob, &private_keys)
            .expect("journal key should decrypt media blob")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{TEST_MASTER_KEY, build_e2e_journal_fixture};
    use super::*;
    use aes_gcm::aead::Generate;
    use rsa::RsaPrivateKey;
    use serde_json::json;

    #[test]
    fn decrypts_existing_aes_gcm_ciphertext() {
        let key = [0_u8; 32];
        let encrypted = "RDEBAAAAAAAAAAAAAAAAAIrGOR0CDg5OZCGoo9uH9HobDGq+TsayvcIyviSU2ir6wHwzm9kAAAAAAAAAAAAAAAAAAAAA";

        assert_eq!(
            decrypt_d1_mode0_with_key(encrypted, &key).expect("existing ciphertext should decrypt"),
            b"Day One compatibility"
        );
    }

    #[test]
    fn encrypt_journal_metadata_round_trips_and_is_idempotent() {
        let fixture = build_e2e_journal_fixture();
        let mut payload = fixture
            .journal
            .as_object()
            .expect("fixture journal is an object")
            .clone();
        payload.insert("name".to_owned(), json!("Trip to Iceland"));
        payload.insert("description_v2".to_owned(), json!("")); // empty -> skipped

        encrypt_journal_metadata_for_push(
            &mut payload,
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .expect("metadata should encrypt");

        // name is now a D1 blob; recover the vault key and confirm it decrypts back.
        let encrypted_name = payload["name"].as_str().expect("name string");
        assert!(is_d1_base64_payload(encrypted_name));
        let vault = payload
            .get("encryption")
            .and_then(|v| v.get("vault"))
            .and_then(Value::as_object)
            .expect("vault present");
        let vault_key = resolve_journal_vault_key(
            vault,
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .expect("vault key resolves");
        let vault_key: [u8; 32] = vault_key.as_slice().try_into().expect("32-byte key");
        let decrypted =
            decrypt_d1_mode0_with_key(encrypted_name, &vault_key).expect("name decrypts");
        assert_eq!(String::from_utf8(decrypted).unwrap(), "Trip to Iceland");

        // Empty field left untouched (web tolerates an empty plaintext description).
        assert_eq!(payload["description_v2"], json!(""));

        // Idempotent: re-running over an already-D1 value does not double-encrypt.
        let once = encrypted_name.to_owned();
        encrypt_journal_metadata_for_push(
            &mut payload,
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .expect("second pass should be a no-op for the encrypted field");
        assert_eq!(payload["name"].as_str().unwrap(), once);
    }

    #[test]
    fn encrypt_journal_metadata_is_noop_for_plaintext_journal() {
        let mut payload = serde_json::Map::new();
        payload.insert("encryption".to_owned(), json!("plaintext"));
        payload.insert("name".to_owned(), json!("Visible Name"));

        encrypt_journal_metadata_for_push(&mut payload, None, None)
            .expect("plaintext journal is a no-op");

        assert_eq!(payload["name"], json!("Visible Name"));
    }

    #[test]
    fn malformed_vault_grant_is_e2e_key_material_unavailable() {
        let fixture = build_e2e_journal_fixture();
        let mut journal = fixture.journal.clone();
        journal["encryption"]["vault"]["grants"][0]["encrypted_vault_key"] =
            json!("definitely-not-base64");

        let err = encrypt_entry_content_for_journal(
            &journal,
            &json!({"id":"entry-1","body":"secret"}),
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .expect_err("malformed local vault grant should fail before encryption");

        assert!(
            is_e2e_key_material_unavailable_error(&err),
            "malformed local vault grants should defer outbox work instead of burning attempts: {err}"
        );
    }

    #[test]
    fn missing_journal_vault_keys_are_e2e_key_material_unavailable() {
        let fixture = build_e2e_journal_fixture();
        let mut journal = fixture.journal.clone();
        journal["encryption"]["vault"]
            .as_object_mut()
            .expect("vault is object")
            .remove("keys");

        let err = encrypt_entry_content_for_journal(
            &journal,
            &json!({"id":"entry-1","body":"secret"}),
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .expect_err("missing local journal vault keys should fail before encryption");

        assert!(
            is_e2e_key_material_unavailable_error(&err),
            "missing local journal vault keys should defer outbox work instead of burning attempts: {err}"
        );
    }

    #[test]
    fn invalid_journal_key_fingerprint_is_e2e_key_material_unavailable() {
        let fixture = build_e2e_journal_fixture();
        let mut journal = fixture.journal.clone();
        journal["encryption"]["vault"]["keys"][0]["fingerprint"] = json!("not-hex");

        let err = encrypt_entry_content_for_journal(
            &journal,
            &json!({"id":"entry-1","body":"secret"}),
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .expect_err("invalid local journal key fingerprint should fail before encryption");

        assert!(
            is_e2e_key_material_unavailable_error(&err),
            "invalid local journal key fingerprint should defer outbox work instead of burning attempts: {err}"
        );
    }

    #[test]
    fn encrypts_original_media_as_locked_key_d1_blob_round_trip() {
        let fixture = build_e2e_journal_fixture();
        let media = b"\xff\xd8\xff\xe0binary-jpeg-bytes\x00\x01\x02\x03not-utf8\xff";

        let ciphertext = encrypt_original_media_for_journal(
            &fixture.journal,
            media,
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .expect("E2E media should encrypt");

        // Server only ever sees a D1 format-1 (locked-key) blob, never plaintext.
        assert!(ciphertext.starts_with(b"D1"));
        assert_eq!(ciphertext[2], 1, "schema should be 1");
        assert_eq!(ciphertext[3], 1, "media should use format 1 (no gzip)");
        assert_ne!(
            &ciphertext[..media.len().min(ciphertext.len())],
            &media[..media.len().min(ciphertext.len())],
            "ciphertext must not begin with plaintext"
        );

        // The embedded fingerprint matches the journal content key.
        let embedded_fingerprint = bytes_to_hex_lower(&ciphertext[4..4 + 32]);
        assert_eq!(embedded_fingerprint, fixture.fingerprint_hex);

        // The journal key decrypts the blob back to the original media bytes.
        let mut private_keys = HashMap::new();
        private_keys.insert(fixture.fingerprint_hex.clone(), fixture.journal_private_key);
        let decrypted = decrypt_d1_mode1_with_private_keys(&ciphertext, &private_keys)
            .expect("journal key should decrypt media");
        assert_eq!(decrypted, media);
    }

    #[test]
    fn encrypt_original_media_errors_for_non_encrypted_journal() {
        let journal = json!({ "id": "plain" });
        let err = encrypt_original_media_for_journal(&journal, b"bytes", None, None)
            .expect_err("plaintext journal should not be media-encrypted");
        assert!(err.to_string().contains("non-encrypted journal"));
    }

    #[test]
    fn journal_is_e2e_encrypted_detects_vault() {
        assert!(journal_is_e2e_encrypted(&json!({
            "encryption": { "vault": { "keys": [] } }
        })));
        assert!(!journal_is_e2e_encrypted(&json!({ "id": "plain" })));
        assert!(!journal_is_e2e_encrypted(&json!({ "encryption": {} })));
    }

    #[test]
    fn decrypts_wrapped_strings_recursively() {
        let ctx = build_crypto_context(Some("key".to_owned()), None);
        let value = json!({
            "a": "ENCRYPTED(hello)",
            "b": ["ENCRYPTED(world)"]
        });
        let out = decrypt_payload(&value, &ctx).expect("decrypt should work");
        assert_eq!(out["a"], "hello");
        assert_eq!(out["b"][0], "world");
    }

    #[test]
    fn user_keys_payload_from_raw_prefers_blob_and_normalizes_known_fields() {
        let payload = json!({
            "encryptedPrivateKey": "root-should-not-win",
            "blob": BASE64_STANDARD.encode(
                serde_json::to_vec(&json!({
                    "activeContentKeyFingerprint": "AA11",
                    "userKey": {
                        "fingerprint": "BB22",
                        "publicKey": "blob-user-public",
                        "encryptedPrivateKey": "blob-user-key"
                    },
                    "otherUserKeys": [{
                        "fingerprint": "CC33",
                        "publicKey": "rotated-user-public",
                        "encryptedPrivateKey": "rotated-user-key"
                    }],
                    "alternativeUserKeys": [{
                        "fingerprint": "DD44",
                        "publicKey": "alternative-user-public",
                        "encryptedPrivateKey": "alternative-user-key"
                    }],
                    "contentKeys": [
                        {
                            "fingerprint": "AA11",
                            "encryptedPrivateKey": "content-encrypted",
                            "publicKey": "-----BEGIN PUBLIC KEY-----blob-----END PUBLIC KEY-----"
                        }
                    ]
                }))
                .expect("blob JSON should serialize")
            )
        });
        let parsed = UserKeysPayload::from_raw(&payload).expect("payload should parse");
        assert_eq!(parsed.encrypted_private_key(), Some("blob-user-key"));
        assert_eq!(parsed.user_public_key(), Some("blob-user-public"));
        assert_eq!(parsed.user_key_fingerprint(), Some("bb22"));
        assert_eq!(parsed.alternative_user_keys().len(), 2);
        assert_eq!(
            parsed.alternative_user_keys()[0].user_key_fingerprint(),
            Some("cc33")
        );
        assert_eq!(
            parsed.alternative_user_keys()[0].user_public_key(),
            Some("rotated-user-public")
        );
        assert_eq!(
            parsed.alternative_user_keys()[1].user_key_fingerprint(),
            Some("dd44")
        );
        assert_eq!(parsed.active_content_key_fingerprint(), Some("aa11"));
        assert_eq!(parsed.content_keys().len(), 1);
        assert_eq!(
            parsed.content_keys()[0].encrypted_private_key(),
            Some("content-encrypted")
        );
        assert_eq!(
            parsed.content_keys()[0].public_key(),
            Some("-----BEGIN PUBLIC KEY-----blob-----END PUBLIC KEY-----")
        );
        assert_eq!(
            parsed
                .active_content_key()
                .and_then(UserContentKey::fingerprint),
            Some("aa11")
        );
    }

    #[test]
    fn user_keys_payload_from_raw_supports_direct_user_key() {
        let payload = json!({
            "fingerprint": "EE55",
            "publicKey": "direct-user-public",
            "encryptedPrivateKey": "direct-user-key"
        });
        let parsed = UserKeysPayload::from_raw(&payload).expect("payload should parse");
        assert_eq!(parsed.encrypted_private_key(), Some("direct-user-key"));
        assert_eq!(parsed.user_public_key(), Some("direct-user-public"));
        assert_eq!(parsed.user_key_fingerprint(), Some("ee55"));
    }

    #[test]
    fn user_keys_payload_from_raw_supports_direct_snake_case_user_key() {
        let payload = json!({
            "fingerprint": "FF66",
            "public_key": "direct-snake-user-public",
            "encrypted_private_key": "direct-snake-user-key"
        });
        let parsed = UserKeysPayload::from_raw(&payload).expect("payload should parse");
        assert_eq!(
            parsed.encrypted_private_key(),
            Some("direct-snake-user-key")
        );
        assert_eq!(parsed.user_public_key(), Some("direct-snake-user-public"));
        assert_eq!(parsed.user_key_fingerprint(), Some("ff66"));
    }

    #[test]
    fn malformed_content_bundle_does_not_drop_user_identity_key() {
        let fixture = build_e2e_journal_fixture();
        let wrapped: Value =
            serde_json::from_str(&fixture.user_keys_json).expect("fixture keys should parse");
        let direct = wrapped["userKey"].to_string();
        let ctx = build_crypto_context_from_sources(
            Some(fixture.master_key),
            Some(&direct),
            Some("not-json"),
        );

        assert!(ctx.d1_content_private_keys.contains_key("aa11"));
    }

    #[test]
    fn user_keys_payload_from_raw_supports_snake_case_without_blob() {
        let payload = json!({
            "user_key": {
                "fingerprint": "CC33",
                "public_key": "root-user-public",
                "encrypted_private_key": "root-user-key"
            },
            "active_content_key_fingerprint": "DD44",
            "content_keys": [
                {
                    "fingerprint": "DD44",
                    "encrypted_private_key": "snake-content-private",
                    "public_key": "snake-public"
                }
            ]
        });
        let parsed = UserKeysPayload::from_raw(&payload).expect("payload should parse");
        assert_eq!(parsed.encrypted_private_key(), Some("root-user-key"));
        assert_eq!(parsed.user_public_key(), Some("root-user-public"));
        assert_eq!(parsed.user_key_fingerprint(), Some("cc33"));
        assert_eq!(parsed.active_content_key_fingerprint(), Some("dd44"));
        assert_eq!(parsed.content_keys().len(), 1);
        assert_eq!(
            parsed
                .active_content_key()
                .and_then(UserContentKey::public_key),
            Some("snake-public")
        );
        assert!(parsed.has_content_keys_field());
    }

    #[test]
    fn active_content_key_error_message_distinguishes_missing_vs_empty_content_keys() {
        let missing_content_keys = json!({
            "activeContentKeyFingerprint": "aa11"
        });
        assert_eq!(
            active_content_key_error_message(Some(&missing_content_keys.to_string())),
            "synced user keys are missing `contentKeys`; run `dayone sync` to refresh keys"
        );

        let empty_content_keys = json!({
            "activeContentKeyFingerprint": "aa11",
            "contentKeys": []
        });
        assert_eq!(
            active_content_key_error_message(Some(&empty_content_keys.to_string())),
            "synced user keys contain no content keys; run `dayone sync` to refresh keys"
        );
    }

    #[test]
    fn active_content_key_error_message_reports_parse_failure_for_invalid_blob_payload() {
        let invalid_blob = json!({
            "blob": "%%%not-base64%%%",
            "activeContentKeyFingerprint": "aa11",
            "contentKeys": [{
                "fingerprint": "aa11",
                "publicKey": "pem"
            }]
        });
        assert_eq!(
            active_content_key_error_message(Some(&invalid_blob.to_string())),
            "failed to parse synced user keys for daily chat encryption; run `dayone sync` to refresh user keys"
        );
        assert!(parse_user_keys_payload(&invalid_blob.to_string()).is_none());
    }

    #[test]
    fn decrypts_d1_json_array_into_value_array() {
        let ctx = build_crypto_context(Some(TEST_MASTER_KEY.to_owned()), None);
        let key =
            derive_d1_symmetric_key_from_master_key(TEST_MASTER_KEY).expect("key should derive");
        let encrypted = encrypt_test_d1_mode0(&key, b"[1,2,3]");
        let out =
            decrypt_payload(&json!({ "embedding": encrypted }), &ctx).expect("decrypt should work");
        assert!(out["embedding"].is_array());
        assert_eq!(out["embedding"][0], 1);
    }

    fn encrypt_test_d1_mode0(key: &[u8; 32], plaintext: &[u8]) -> String {
        let key = <&aes_gcm::Key<Aes256Gcm>>::from(key);
        let cipher = Aes256Gcm::new(key);
        let nonce = aes_gcm::aead::Nonce::<Aes256Gcm>::generate();
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .expect("encryption should succeed");

        let mut d1 = Vec::new();
        d1.extend_from_slice(b"D1");
        d1.push(1); // schema
        d1.push(0); // version
        d1.extend_from_slice(&nonce);
        d1.extend_from_slice(&ciphertext);
        d1.extend_from_slice(&[0_u8; 16]); // checksum placeholder
        BASE64_STANDARD.encode(d1)
    }

    #[test]
    fn encrypts_entry_payload_mode2_with_md5_suffix() {
        let mut rng = OsRng;
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key should generate");
        let public = private.to_public_key();
        let fingerprint_hex =
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_owned();
        let plaintext = br#"{"id":"ABC","body":"hello"}"#;

        let encrypted = encrypt_entry_payload_d1_mode2(plaintext, &fingerprint_hex, &public)
            .expect("encryption should succeed");
        assert!(encrypted.starts_with(b"D1"));
        assert_eq!(encrypted[2], 1);
        assert_eq!(encrypted[3], 2);

        let md5_start = encrypted.len() - 16;
        let mut md5 = Md5::new();
        md5.update(&encrypted[..md5_start]);
        assert_eq!(&encrypted[md5_start..], md5.finalize().as_slice());

        let mut offset = 4usize;
        offset += 32; // fingerprint
        let sig_len = ((encrypted[offset] as usize) << 8) | (encrypted[offset + 1] as usize);
        assert_eq!(sig_len, 0);
        offset += 2;
        let wrapped_len = private.size();
        let wrapped_key = &encrypted[offset..offset + wrapped_len];
        offset += wrapped_len;
        let iv = &encrypted[offset..offset + 12];
        offset += 12;
        let ciphertext_and_tag = &encrypted[offset..md5_start];

        let content_key = private
            .decrypt(Oaep::new::<Sha1>(), wrapped_key)
            .expect("wrapped key should decrypt");
        let key = <&aes_gcm::Key<Aes256Gcm>>::try_from(content_key.as_slice())
            .expect("content key should be 32 bytes");
        let nonce =
            <&aes_gcm::aead::Nonce<Aes256Gcm>>::try_from(iv).expect("nonce should be 12 bytes");
        let cipher = Aes256Gcm::new(key);
        let compressed = cipher
            .decrypt(nonce, ciphertext_and_tag)
            .expect("ciphertext should decrypt");
        let mut decoder = GzDecoder::new(compressed.as_slice());
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .expect("gzip decode should succeed");
        assert_eq!(out, plaintext);
    }

    #[test]
    fn encrypt_and_decrypt_daily_chat_payload_round_trip() {
        let mut rng = OsRng;
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key should generate");
        let public = private.to_public_key();
        let fingerprint_hex =
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_owned();
        let mut private_keys = HashMap::new();
        private_keys.insert(fingerprint_hex.clone(), private);
        let ctx = CryptoContext {
            master_key: Some("D1-test-master-key".to_owned()),
            d1_symmetric_key: None,
            d1_content_private_keys: private_keys,
            active_content_public_key: Some((fingerprint_hex, public)),
            active_content_public_key_error: None,
        };

        let payload = json!({
            "date": "2026-03-17",
            "chat_history": [
                { "id": "m1", "role": "user", "content": "Hello!", "timestamp": "2026-03-17T12:00:00.000Z" },
                { "id": "m2", "role": "meta", "content": "log_mode_on", "timestamp": "2026-03-17T12:00:10.000Z" }
            ],
            "context": {
                "mood": "happy",
                "stress": 2
            },
            "shared_message_key": Value::Null
        });

        let encrypted = encrypt_payload(payload, &ctx).expect("encrypt should succeed");
        let user_content = encrypted["chat_history"][0]["content"]
            .as_str()
            .expect("encrypted user content should be string");
        assert!(user_content.starts_with("RD"));
        let meta_content = encrypted["chat_history"][1]["content"]
            .as_str()
            .expect("meta content should be string");
        assert_eq!(meta_content, "log_mode_on");
        assert!(
            encrypted["shared_message_key"]
                .as_str()
                .expect("shared key should be string")
                .starts_with("RD")
        );

        let decrypted = decrypt_payload(&encrypted, &ctx).expect("decrypt should succeed");
        assert_eq!(decrypted["chat_history"][0]["content"], "Hello!");
        assert_eq!(decrypted["chat_history"][1]["content"], "log_mode_on");
        assert_eq!(decrypted["context"]["mood"], "happy");
        assert_eq!(decrypted["context"]["stress"], 2);
        assert!(
            decrypted["shared_message_key"]
                .as_str()
                .expect("shared key should remain encrypted")
                .starts_with("RD")
        );
    }

    #[test]
    fn encrypt_daily_chat_fails_when_shared_key_invalid_for_encrypted_history() {
        let mut rng = OsRng;
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key should generate");
        let public = private.to_public_key();
        let fingerprint_hex =
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_owned();
        let ctx = CryptoContext {
            master_key: None,
            d1_symmetric_key: None,
            d1_content_private_keys: HashMap::new(),
            active_content_public_key: Some((fingerprint_hex, public)),
            active_content_public_key_error: None,
        };
        let payload = json!({
            "date": "2026-03-17",
            "chat_history": [
                { "id": "m1", "role": "user", "content": BASE64_STANDARD.encode(b"D1\x01\x00invalid"), "timestamp": "2026-03-17T12:00:00.000Z" }
            ],
            "context": {},
            "shared_message_key": "not-base64"
        });
        let err = encrypt_payload(payload, &ctx).expect_err("encrypt should fail");
        assert!(err.to_string().contains("shared_message_key is invalid"));
    }

    #[test]
    fn decode_shared_key_falls_back_to_raw_base64_when_not_d1() {
        let ctx = CryptoContext::default();
        let mut raw = [0_u8; 32];
        raw[0] = b'D';
        raw[1] = b'0';
        let encoded = BASE64_STANDARD.encode(raw);
        assert!(encoded.starts_with("RD"));

        let parsed =
            decode_shared_key(&Value::String(encoded), &ctx).expect("raw key should parse");
        assert_eq!(parsed, raw);
    }

    #[test]
    fn encrypt_daily_chat_fails_when_encrypted_history_has_no_shared_key() {
        let mut rng = OsRng;
        let public = RsaPrivateKey::new(&mut rng, 2048)
            .expect("rsa key should generate")
            .to_public_key();
        let fingerprint_hex =
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_owned();
        let ctx = CryptoContext {
            master_key: None,
            d1_symmetric_key: None,
            d1_content_private_keys: HashMap::new(),
            active_content_public_key: Some((fingerprint_hex, public)),
            active_content_public_key_error: None,
        };
        let payload = json!({
            "date": "2026-03-17",
            "chat_history": [
                { "id": "m1", "role": "user", "content": BASE64_STANDARD.encode(b"D1\x01\x00alreadyencrypted"), "timestamp": "2026-03-17T12:00:00.000Z" }
            ],
            "context": {}
        });
        let err = encrypt_payload(payload, &ctx).expect_err("encrypt should fail");
        assert!(err.to_string().contains(
            "shared_message_key is required when daily chat history is already encrypted"
        ));
    }

    #[test]
    fn encrypt_daily_chat_fails_when_encrypted_message_cannot_be_decrypted_by_shared_key() {
        let mut rng = OsRng;
        let public = RsaPrivateKey::new(&mut rng, 2048)
            .expect("rsa key should generate")
            .to_public_key();
        let fingerprint_hex =
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_owned();
        let ctx = CryptoContext {
            master_key: None,
            d1_symmetric_key: None,
            d1_content_private_keys: HashMap::new(),
            active_content_public_key: Some((fingerprint_hex, public)),
            active_content_public_key_error: None,
        };
        let payload = json!({
            "date": "2026-03-17",
            "chat_history": [
                { "id": "m1", "role": "user", "content": BASE64_STANDARD.encode(b"D1\x01\x00invalid"), "timestamp": "2026-03-17T12:00:00.000Z" }
            ],
            "context": {},
            "shared_message_key": BASE64_STANDARD.encode([7_u8; 32])
        });
        let err = encrypt_payload(payload, &ctx).expect_err("encrypt should fail");
        assert!(err.to_string().contains("mismatched shared_message_key"));
    }
}
